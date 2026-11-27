package com.pyn.ime

import android.content.Intent
import android.content.res.Configuration
import android.inputmethodservice.InputMethodService
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.text.InputType
import android.util.Log
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.View
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.widget.FrameLayout
import android.widget.LinearLayout

/**
 * Threading: the UI thread only handles touches and draws. Every engine call and every
 * InputConnection edit runs on [worker] in order; results come back to the UI thread through
 * [mainHandler] to refresh the candidate bar and keyboard. Nothing on the UI thread calls into the
 * native engine while typing, so a slow decode can no longer drop frames.
 *
 * Fields marked "engine thread" are written only on the worker ([lastComposingLen] is also read by
 * [onUpdateSelection] as a hint, hence volatile).
 */
class PynImeService : InputMethodService() {

    companion object {
        private const val TAG = "PynImeService"
        /** Characters `ime-ipc::session` maps to Chinese punctuation (see `is_mapped_punct`). */
        private const val ENGINE_MAPPED_PUNCT = ",.?!;:()<>\\\"'^_"
    }

    private lateinit var prefs: Prefs
    private val worker = EngineWorker()
    private val mainHandler = Handler(Looper.getMainLooper())

    private var rootLayout: LinearLayout? = null
    private var candidateBar: CandidateBarView? = null
    private var qwertyView: QwertyKeyboardView? = null
    private var symbolView: SymbolKeyboardView? = null
    private var bodyContainer: FrameLayout? = null

    // UI thread state.
    private var isChineseMode: Boolean = true
    private var showingSymbols: Boolean = false

    // Engine thread state.
    @Volatile
    private var lastComposingLen: Int = 0
    @Volatile
    private var lastComposingEditMs: Long = 0L
    @Volatile
    private var currentImeAction: Int = EditorInfo.IME_ACTION_NONE

    /** Engine readiness without touching JNI (which would wait for an in-flight decode). */
    private fun engineReady(): Boolean = ModelManager.state is ModelManager.State.Ready

    private val modelListener: (ModelManager.State) -> Unit = { st ->
        when (st) {
            is ModelManager.State.Idle -> candidateBar?.setStatus("")
            is ModelManager.State.Loading -> candidateBar?.setStatus(st.stage)
            is ModelManager.State.Ready -> {
                worker.post { prefs.syncToEngine() }
                candidateBar?.setStatus("")
            }
            is ModelManager.State.Error -> candidateBar?.setStatus("加载失败: ${st.message}")
        }
    }

    override fun onCreate() {
        super.onCreate()
        prefs = Prefs(this)
        ModelManager.addListener(modelListener)
        ModelManager.ensureInitialized(this)
    }

    override fun onDestroy() {
        ModelManager.removeListener(modelListener)
        worker.post { if (NativeEngine.isReady()) NativeEngine.saveUserDict() }
        worker.quitSafely()
        super.onDestroy()
    }

    override fun onCreateInputView(): View {
        val bar = CandidateBarView(this)
        val qwerty = QwertyKeyboardView(this)
        val symbols = SymbolKeyboardView(this).apply { visibility = View.GONE }

        val container = FrameLayout(this).apply {
            clipChildren = false
            clipToPadding = false
            addView(qwerty, FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT,
                FrameLayout.LayoutParams.WRAP_CONTENT
            ))
            addView(symbols, FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT,
                FrameLayout.LayoutParams.WRAP_CONTENT
            ))
            addView(bar.expandedPanel, FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT,
                (224f * resources.displayMetrics.density).toInt()
            ))
        }

        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            clipChildren = false
            clipToPadding = false
            addView(bar, LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                LinearLayout.LayoutParams.WRAP_CONTENT
            ))
            addView(container, LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                LinearLayout.LayoutParams.WRAP_CONTENT
            ))
        }

        bar.listener = object : CandidateBarView.Listener {
            override fun onPickCandidate(index: Int) {
                if (!engineReady()) return
                val ic = currentInputConnection
                worker.post {
                    applyAction(ic, NativeEngine.pickCandidate(index, readContextBefore(ic)))
                }
            }

            override fun onToggleExpand(expanded: Boolean) {
                bar.expandedPanel.visibility = if (expanded) View.VISIBLE else View.GONE
                qwerty.visibility = if (expanded || showingSymbols) View.GONE else View.VISIBLE
                symbols.visibility = if (!expanded && showingSymbols) View.VISIBLE else View.GONE
            }

            override fun onToggleSingleChar() {
                if (!engineReady()) return
                val ic = currentInputConnection
                worker.post {
                    applyAction(
                        ic,
                        NativeEngine.handleKey(NativeEngine.KEY_TOGGLE_SINGLE_CHAR, readContextBefore(ic))
                    )
                }
            }

            override fun onOpenSettings() {
                val intent = Intent(this@PynImeService, SettingsActivity::class.java).apply {
                    addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                }
                startActivity(intent)
            }
        }

        qwerty.listener = object : QwertyKeyboardView.Listener {
            override fun onLetter(ch: Char, isUpper: Boolean) {
                val ic = currentInputConnection
                val out = (if (isUpper) ch.uppercaseChar() else ch).toString()
                if (!engineReady()) {
                    ic?.commitText(out, 1)
                    return
                }
                val handle = { decode: Boolean ->
                    // A deferred letter only extends the keys, so it needs no context round trip.
                    val ctx = if (decode) readContextBefore(ic) else ""
                    val res = NativeEngine.handleChar(ch, isUpper, ctx, decode)
                    if (res.action == "pass_through") {
                        // English mode / Shift+letter: the engine defers to the host; type it ourselves.
                        commitPassThrough(ic, out, res)
                    } else {
                        applyAction(ic, res, showCandidates = decode)
                    }
                }
                if (isUpper) worker.post { handle(true) } else worker.postLetter(handle)
            }

            override fun onSeparator() {
                val ic = currentInputConnection
                if (!engineReady()) {
                    ic?.commitText("'", 1)
                    return
                }
                worker.post {
                    val res = NativeEngine.handleKey(NativeEngine.KEY_APOSTROPHE, readContextBefore(ic))
                    if (res.action == "pass_through") commitPassThrough(ic, "'", res) else applyAction(ic, res)
                }
            }

            override fun onBackspace() {
                val ic = currentInputConnection
                if (!engineReady()) {
                    sendBackspace(ic)
                    return
                }
                worker.post {
                    val res = NativeEngine.handleKey(NativeEngine.KEY_BACKSPACE, readContextBefore(ic))
                    if (res.action == "pass_through") sendBackspace(ic)
                    applyAction(ic, res)
                }
            }

            override fun onSpace() {
                val ic = currentInputConnection
                if (!engineReady()) {
                    ic?.commitText(" ", 1)
                    return
                }
                worker.post {
                    val res = NativeEngine.handleKey(NativeEngine.KEY_SPACE, readContextBefore(ic))
                    if (res.action == "pass_through") ic?.commitText(" ", 1)
                    applyAction(ic, res)
                }
            }

            override fun onEnter() {
                val ic = currentInputConnection
                if (!engineReady()) {
                    performEnterAction(ic)
                    return
                }
                worker.post {
                    val res = NativeEngine.handleKey(NativeEngine.KEY_ENTER, readContextBefore(ic))
                    if (res.action != "pass_through") applyAction(ic, res) else performEnterAction(ic)
                }
            }

            override fun onPunct(ch: Char) {
                val ic = currentInputConnection
                if (!engineReady()) {
                    ic?.commitText(ch.toString(), 1)
                    return
                }
                worker.post {
                    val res = NativeEngine.handleChar(ch, false, readContextBefore(ic))
                    if (res.action == "pass_through") commitPassThrough(ic, ch.toString(), res) else applyAction(ic, res)
                }
            }

            override fun onToggleMode() {
                isChineseMode = !isChineseMode
                syncModeToViews()
                if (!engineReady()) return
                val ic = currentInputConnection
                val chinese = isChineseMode
                worker.post { applyAction(ic, NativeEngine.setMode(chinese, readContextBefore(ic))) }
            }

            override fun onQuickSymbol(ch: Char) {
                // Punctuation the engine maps (，。？！（）“” …) keeps the normal punctuation path,
                // including committing the top candidate first while composing.
                if (ch in ENGINE_MAPPED_PUNCT) {
                    onPunct(ch)
                    return
                }
                // Digits and other symbols are literal; while composing, commit the top candidate first.
                commitLiteral(currentInputConnection, ch.toString())
            }

            override fun onOpenSymbols() {
                showingSymbols = true
                bar.collapseIfExpanded()
                symbols.isChineseMode = isChineseMode
                symbols.resetPage()
                qwerty.visibility = View.GONE
                symbols.visibility = View.VISIBLE
            }
        }

        symbols.listener = object : SymbolKeyboardView.Listener {
            override fun onSymbol(text: String) {
                commitLiteral(currentInputConnection, text)
            }

            override fun onBackspace() {
                val ic = currentInputConnection
                if (!engineReady()) {
                    sendBackspace(ic)
                    return
                }
                worker.post {
                    if (lastComposingLen > 0) {
                        val res = NativeEngine.handleKey(NativeEngine.KEY_BACKSPACE, readContextBefore(ic))
                        if (res.action == "pass_through") sendBackspace(ic)
                        applyAction(ic, res)
                    } else {
                        sendBackspace(ic)
                    }
                }
            }

            override fun onSpace() {
                val ic = currentInputConnection
                if (!engineReady()) {
                    ic?.commitText(" ", 1)
                    return
                }
                worker.post {
                    if (lastComposingLen > 0) {
                        val res = NativeEngine.handleKey(NativeEngine.KEY_SPACE, readContextBefore(ic))
                        if (res.action == "pass_through") ic?.commitText(" ", 1)
                        applyAction(ic, res)
                    } else {
                        ic?.commitText(" ", 1)
                    }
                }
            }

            override fun onEnter() {
                val ic = currentInputConnection
                if (!engineReady()) {
                    performEnterAction(ic)
                    return
                }
                worker.post {
                    if (lastComposingLen > 0) {
                        val res = NativeEngine.handleKey(NativeEngine.KEY_ENTER, readContextBefore(ic))
                        if (res.action != "pass_through") {
                            applyAction(ic, res)
                            return@post
                        }
                    }
                    performEnterAction(ic)
                }
            }

            override fun onBackToLetters() {
                showingSymbols = false
                symbols.visibility = View.GONE
                qwerty.visibility = View.VISIBLE
            }
        }

        candidateBar = bar
        qwertyView = qwerty
        symbolView = symbols
        bodyContainer = container
        rootLayout = root

        applyPrefsToViews()
        modelListener(ModelManager.state)
        return root
    }

    override fun onStartInputView(info: EditorInfo?, restarting: Boolean) {
        super.onStartInputView(info, restarting)
        ModelManager.ensureInitialized(this)
        applyPrefsToViews()

        showingSymbols = false
        candidateBar?.collapseIfExpanded()
        symbolView?.visibility = View.GONE
        qwertyView?.visibility = View.VISIBLE
        qwertyView?.resetShiftIfOnce()
        qwertyView?.isComposing = false

        isChineseMode = !shouldDefaultEnglish(info)
        syncModeToViews()
        updateEnterActionLabel(info)
        candidateBar?.setCandidates(false, "", emptyList(), false)

        val chinese = isChineseMode
        worker.post {
            lastComposingLen = 0
            if (NativeEngine.isReady()) {
                prefs.syncToEngine()
                NativeEngine.resetSession()
                NativeEngine.setMode(chinese, "")
            }
        }
    }

    override fun onFinishInputView(finishingInput: Boolean) {
        super.onFinishInputView(finishingInput)
        val ic = currentInputConnection
        qwertyView?.isComposing = false
        candidateBar?.setCandidates(false, "", emptyList(), false)
        worker.post {
            if (NativeEngine.isReady()) {
                if (lastComposingLen > 0) ic?.finishComposingText()
                NativeEngine.resetSession()
                NativeEngine.saveUserDict()
            }
            lastComposingLen = 0
        }
    }

    override fun onEvaluateFullscreenMode(): Boolean = false

    override fun onUpdateSelection(
        oldSelStart: Int,
        oldSelEnd: Int,
        newSelStart: Int,
        newSelEnd: Int,
        candidatesStart: Int,
        candidatesEnd: Int
    ) {
        super.onUpdateSelection(
            oldSelStart, oldSelEnd, newSelStart, newSelEnd, candidatesStart, candidatesEnd
        )
        // Ignore transient callbacks fired immediately after our own setComposingText.
        val composingLen = lastComposingLen
        if (composingLen <= 0) return
        if (SystemClock.uptimeMillis() - lastComposingEditMs < 200L) return

        val cursorLeftSpan = candidatesStart >= 0 && candidatesEnd >= 0 &&
            (candidatesEnd - candidatesStart) == composingLen &&
            (newSelStart < candidatesStart || newSelStart > candidatesEnd)
        val spanClearedExternally = candidatesStart == -1 && candidatesEnd == -1 &&
            (oldSelStart != newSelStart || oldSelEnd != newSelEnd)

        if (cursorLeftSpan || spanClearedExternally) {
            Log.i(
                TAG,
                "reset on selection jump sel=$newSelStart..$newSelEnd cand=$candidatesStart..$candidatesEnd len=$composingLen"
            )
            val ic = currentInputConnection
            worker.post {
                // Skip if typing moved on since this callback was generated.
                if (lastComposingLen != composingLen) return@post
                ic?.finishComposingText()
                if (NativeEngine.isReady()) NativeEngine.resetSession()
                lastComposingLen = 0
                mainHandler.post {
                    qwertyView?.isComposing = false
                    candidateBar?.setCandidates(false, "", emptyList(), false)
                }
            }
        }
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        candidateBar?.refreshTheme()
        qwertyView?.refreshTheme()
        symbolView?.refreshTheme()
    }

    private fun applyPrefsToViews() {
        qwertyView?.hapticEnabled = prefs.hapticEnabled
        qwertyView?.keyPreviewEnabled = prefs.keyPreviewEnabled
        symbolView?.hapticEnabled = prefs.hapticEnabled
    }

    private fun syncModeToViews() {
        qwertyView?.isChineseMode = isChineseMode
        symbolView?.isChineseMode = isChineseMode
    }

    private fun shouldDefaultEnglish(info: EditorInfo?): Boolean {
        if (info == null) return false
        val cls = info.inputType and InputType.TYPE_MASK_CLASS
        val variation = info.inputType and InputType.TYPE_MASK_VARIATION
        if (cls == InputType.TYPE_CLASS_NUMBER || cls == InputType.TYPE_CLASS_PHONE) {
            return true
        }
        return when (variation) {
            InputType.TYPE_TEXT_VARIATION_PASSWORD,
            InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD,
            InputType.TYPE_TEXT_VARIATION_WEB_PASSWORD,
            InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS,
            InputType.TYPE_TEXT_VARIATION_WEB_EMAIL_ADDRESS,
            InputType.TYPE_TEXT_VARIATION_URI -> true
            else -> false
        }
    }

    private fun updateEnterActionLabel(info: EditorInfo?) {
        val imeOptions = info?.imeOptions ?: 0
        val noAction = (imeOptions and EditorInfo.IME_FLAG_NO_ENTER_ACTION) != 0
        val action = if (noAction) EditorInfo.IME_ACTION_NONE else (imeOptions and EditorInfo.IME_MASK_ACTION)
        currentImeAction = action
        val label = when (action) {
            EditorInfo.IME_ACTION_SEARCH -> "搜索"
            EditorInfo.IME_ACTION_SEND -> "发送"
            EditorInfo.IME_ACTION_GO -> "前往"
            EditorInfo.IME_ACTION_NEXT -> "下一项"
            EditorInfo.IME_ACTION_DONE -> "完成"
            else -> "换行"
        }
        qwertyView?.enterLabel = label
        symbolView?.enterLabel = label
    }

    // ---------------------------------------------------------------- engine thread helpers

    /** Engine thread: text before the caret, excluding our own composing span. */
    private fun readContextBefore(ic: InputConnection?): String {
        ic ?: return ""
        // Fetch slightly more text so stripping current composing span still leaves >= 24 chars of context.
        val composingLen = lastComposingLen
        val raw = ic.getTextBeforeCursor(40 + composingLen, 0)?.toString() ?: return ""
        val trimmed = if (composingLen > 0 && raw.length >= composingLen) {
            raw.dropLast(composingLen)
        } else {
            raw
        }
        return if (trimmed.length > 32) trimmed.takeLast(32) else trimmed
    }

    /**
     * Engine thread: apply [res] to the editor, then hand the visual update to the UI thread.
     * [showCandidates] is false for a deferred letter, whose candidate list is not current.
     */
    private fun applyAction(ic: InputConnection?, res: ActionResult, showCandidates: Boolean = true) {
        var ended = false
        when (res.action) {
            "update" -> {
                lastComposingLen = res.inlinePreedit.length
                lastComposingEditMs = SystemClock.uptimeMillis()
                ic?.setComposingText(res.inlinePreedit, 1)
                if (showCandidates) {
                    Log.i(
                        TAG,
                        "update mode=${if (res.isChinese) "zh" else "en"} preeditLen=${res.inlinePreedit.length} cands=${res.candidates.size} lat=${"%.1f".format(res.latencyMs)}ms"
                    )
                }
            }
            "commit" -> {
                lastComposingLen = 0
                val text = res.commitText ?: ""
                if (text.isNotEmpty()) ic?.commitText(text, 1) else ic?.finishComposingText()
                ended = true
            }
            "cancel" -> {
                lastComposingLen = 0
                ic?.setComposingText("", 1)
                ic?.finishComposingText()
                ended = true
            }
            else -> {
                if (!res.composing && lastComposingLen > 0) {
                    lastComposingLen = 0
                    ic?.finishComposingText()
                    ended = true
                }
            }
        }
        mainHandler.post {
            isChineseMode = res.isChinese
            syncModeToViews()
            if (res.action == "update") {
                qwertyView?.isComposing = res.composing
                if (showCandidates) {
                    candidateBar?.setCandidates(
                        composing = res.composing,
                        panelPreedit = res.panelPreedit,
                        candidates = res.candidates,
                        singleCharMode = res.singleCharMode
                    )
                }
            } else if (ended) {
                qwertyView?.isComposing = false
                candidateBar?.setCandidates(false, "", emptyList(), false)
            }
        }
    }

    /** Engine thread: finish any composing span first (so it isn't replaced), then insert raw key text. */
    private fun commitPassThrough(ic: InputConnection?, text: String, res: ActionResult) {
        applyAction(ic, res)
        if (lastComposingLen > 0) {
            lastComposingLen = 0
            ic?.finishComposingText()
        }
        ic?.commitText(text, 1)
    }

    /** Literal text (digits, symbols): while composing, the top candidate is committed first. */
    private fun commitLiteral(ic: InputConnection?, text: String) {
        if (!engineReady()) {
            ic?.commitText(text, 1)
            return
        }
        worker.post {
            if (lastComposingLen > 0) {
                applyAction(ic, NativeEngine.commitWithSuffix(text, readContextBefore(ic)))
            } else {
                ic?.commitText(text, 1)
            }
        }
    }

    private fun sendBackspace(ic: InputConnection?) {
        ic ?: return
        val selected = ic.getSelectedText(0)
        if (!selected.isNullOrEmpty()) {
            ic.commitText("", 1)
        } else {
            ic.deleteSurroundingText(1, 0)
        }
    }

    private fun performEnterAction(ic: InputConnection?) {
        ic ?: return
        val action = currentImeAction
        if (action != EditorInfo.IME_ACTION_NONE && action != EditorInfo.IME_ACTION_UNSPECIFIED) {
            if (ic.performEditorAction(action)) return
        }
        val now = SystemClock.uptimeMillis()
        val flags = KeyEvent.FLAG_SOFT_KEYBOARD or KeyEvent.FLAG_KEEP_TOUCH_MODE
        ic.sendKeyEvent(
            KeyEvent(now, now, KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_ENTER, 0, 0, KeyCharacterMap.VIRTUAL_KEYBOARD, 0, flags)
        )
        ic.sendKeyEvent(
            KeyEvent(now, now, KeyEvent.ACTION_UP, KeyEvent.KEYCODE_ENTER, 0, 0, KeyCharacterMap.VIRTUAL_KEYBOARD, 0, flags)
        )
    }
}
