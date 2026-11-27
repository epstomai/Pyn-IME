package com.pyn.ime

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RectF
import android.graphics.Typeface
import android.os.Handler
import android.os.Looper
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import kotlin.math.abs

class QwertyKeyboardView(context: Context) : View(context) {

    enum class ShiftState { OFF, ONCE, LOCKED }

    enum class KeyType {
        LETTER, SHIFT_OR_SEP, BACKSPACE, SYMBOLS, COMMA, MODE, SPACE, PERIOD, ENTER
    }

    /** `alt` is the quick-input digit/symbol, typed by swiping up on the key or long-pressing it. */
    class KeyDef(
        val type: KeyType,
        val code: Char = '\u0000',
        val weight: Float = 1f,
        val alt: Char? = null,
        val rect: RectF = RectF()
    )

    /** Per-finger state: the key it went down on, the key it is over now, and quick-symbol state. */
    private class Touch(val origKey: KeyDef, val downX: Float, val downY: Float) {
        var key: KeyDef = origKey
        var swipeAlt = false
        var altFired = false
    }

    interface Listener {
        fun onLetter(ch: Char, isUpper: Boolean)
        fun onSeparator()
        fun onBackspace()
        fun onSpace()
        fun onEnter()
        fun onPunct(ch: Char)
        fun onToggleMode()
        fun onOpenSymbols()
        fun onQuickSymbol(ch: Char)
    }

    var listener: Listener? = null
    var hapticEnabled: Boolean = true
    var keyPreviewEnabled: Boolean = true

    var isChineseMode: Boolean = true
        set(value) {
            if (field != value) {
                field = value
                if (value) shiftState = ShiftState.OFF
                invalidate()
            }
        }

    var isComposing: Boolean = false
        set(value) {
            if (field != value) {
                field = value
                invalidate()
            }
        }

    var enterLabel: String = "换行"
        set(value) {
            if (field != value) {
                field = value
                invalidate()
            }
        }

    private var shiftState: ShiftState = ShiftState.OFF
    private var lastShiftTapMs: Long = 0L
    private var palette = KeyboardTheme.current(context)

    private fun letterRow(letters: String, alts: String): List<KeyDef> =
        letters.mapIndexed { i, ch -> KeyDef(KeyType.LETTER, ch, 1f, alts[i]) }

    private val rows: List<List<KeyDef>> = listOf(
        letterRow("qwertyuiop", "1234567890"),
        letterRow("asdfghjkl", "@#$%&-+()"),
        buildList {
            add(KeyDef(KeyType.SHIFT_OR_SEP, '\u0000', 1.5f))
            addAll(letterRow("zxcvbnm", "*\"/:;!?"))
            add(KeyDef(KeyType.BACKSPACE, '\u0000', 1.5f))
        },
        listOf(
            KeyDef(KeyType.SYMBOLS, '\u0000', 1.45f),
            KeyDef(KeyType.COMMA, ',', 0.95f),
            KeyDef(KeyType.MODE, '\u0000', 1.15f),
            KeyDef(KeyType.SPACE, ' ', 3.9f),
            KeyDef(KeyType.PERIOD, '.', 0.95f),
            KeyDef(KeyType.ENTER, '\n', 1.6f)
        )
    )

    private val touches = mutableMapOf<Int, Touch>()
    private val longPressRunnables = mutableMapOf<Int, Runnable>()
    private val handler = Handler(Looper.getMainLooper())
    private var repeatingBackspace = false
    private val backspaceRepeater = object : Runnable {
        override fun run() {
            if (!repeatingBackspace) return
            listener?.onBackspace()
            handler.postDelayed(this, 55L)
        }
    }

    private val density = resources.displayMetrics.density
    private val swipeThreshold = 20f * density
    private val longPressMs = 300L
    private val keyPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val shadowPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val textPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        textAlign = Paint.Align.CENTER
        typeface = Typeface.create("sans-serif", Typeface.NORMAL)
    }
    private val subTextPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        textAlign = Paint.Align.CENTER
        typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
    }
    private val hintPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        textAlign = Paint.Align.RIGHT
        typeface = Typeface.create("sans-serif", Typeface.NORMAL)
    }
    private val popupPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val popupRect = RectF()
    private val tmpRect = RectF()

    fun refreshTheme() {
        palette = KeyboardTheme.current(context)
        invalidate()
    }

    fun resetShiftIfOnce() {
        if (shiftState == ShiftState.ONCE) {
            shiftState = ShiftState.OFF
            invalidate()
        }
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        val w = MeasureSpec.getSize(widthMeasureSpec)
        val desiredH = (224f * density).toInt()
        setMeasuredDimension(w, resolveSize(desiredH, heightMeasureSpec))
    }

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        layoutKeys(w.toFloat(), h.toFloat())
    }

    private fun layoutKeys(width: Float, height: Float) {
        val padH = 4f * density
        val padTop = 6f * density
        val padBottom = 8f * density
        val gapX = 5f * density
        val gapY = 8f * density

        val availH = (height - padTop - padBottom).coerceAtLeast(1f)
        val rowH = (availH - gapY * (rows.size - 1)) / rows.size

        for ((rIdx, row) in rows.withIndex()) {
            val top = padTop + rIdx * (rowH + gapY)
            val bottom = top + rowH
            val totalWeight = row.sumOf { it.weight.toDouble() }.toFloat()

            // Row 2 (asdfghjkl) has half-key side inset like standard QWERTY.
            val sideInset = if (rIdx == 1) (width - 2f * padH) * 0.05f else 0f
            val rowLeft = padH + sideInset
            val rowWidth = (width - 2f * padH - 2f * sideInset).coerceAtLeast(1f)
            val usableW = rowWidth - gapX * (row.size - 1)

            var x = rowLeft
            for (k in row) {
                val kw = usableW * (k.weight / totalWeight)
                k.rect.set(x, top, x + kw, bottom)
                x += kw + gapX
            }
        }
    }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        // Parents use clipChildren=false (for key previews), so an unclipped drawColor would
        // flood the whole IME window and paint over the candidate bar. Clip to our own bounds.
        canvas.save()
        canvas.clipRect(0, 0, width, height)
        canvas.drawColor(palette.bg)
        canvas.restore()

        val radius = 6.5f * density
        val shadowOff = 1.2f * density
        val pressedKeys = touches.values.map { it.key }

        for (row in rows) {
            for (k in row) {
                val isPressed = pressedKeys.any { it === k }
                val bgColor = when (k.type) {
                    KeyType.LETTER, KeyType.SPACE -> if (isPressed) palette.keyPressedBg else palette.keyBg
                    KeyType.ENTER -> if (isPressed) palette.accentPressedBg else palette.accentBg
                    KeyType.SHIFT_OR_SEP -> {
                        if (shiftState != ShiftState.OFF && !(isChineseMode && isComposing)) {
                            if (isPressed) palette.accentPressedBg else palette.accentBg
                        } else {
                            if (isPressed) palette.funcKeyPressedBg else palette.funcKeyBg
                        }
                    }
                    else -> if (isPressed) palette.funcKeyPressedBg else palette.funcKeyBg
                }

                // Subtle bottom shadow
                shadowPaint.color = palette.keyShadow
                tmpRect.set(k.rect.left, k.rect.top + shadowOff, k.rect.right, k.rect.bottom + shadowOff)
                canvas.drawRoundRect(tmpRect, radius, radius, shadowPaint)

                keyPaint.color = bgColor
                canvas.drawRoundRect(k.rect, radius, radius, keyPaint)

                drawKeyLabel(canvas, k)
            }
        }

        // Key preview popups; a quick symbol in progress is always previewed (highlighted).
        for (t in touches.values) {
            val k = t.key
            if (k.type != KeyType.LETTER) continue
            val alt = k.alt
            if ((t.swipeAlt || t.altFired) && alt != null) {
                drawPopup(canvas, k, alt.toString(), highlight = true)
            } else if (keyPreviewEnabled) {
                val useUpper = (!isChineseMode || !isComposing) && shiftState != ShiftState.OFF
                drawPopup(canvas, k, (if (useUpper) k.code.uppercaseChar() else k.code).toString(), highlight = false)
            }
        }
    }

    private fun drawKeyLabel(canvas: Canvas, k: KeyDef) {
        val cx = k.rect.centerX()
        val cy = k.rect.centerY()
        val useUpper = !isChineseMode && shiftState != ShiftState.OFF ||
            (isChineseMode && !isComposing && shiftState != ShiftState.OFF)

        val (label, sizeSp, color) = when (k.type) {
            KeyType.LETTER -> {
                val ch = if (useUpper) k.code.uppercaseChar() else k.code
                Triple(ch.toString(), 20f, palette.keyText)
            }
            KeyType.SHIFT_OR_SEP -> {
                if (isChineseMode && isComposing) {
                    Triple("分词'", 13f, palette.keyText)
                } else {
                    val sym = when (shiftState) {
                        ShiftState.OFF -> "⇧"
                        ShiftState.ONCE -> "⬆"
                        ShiftState.LOCKED -> "⇪"
                    }
                    val col = if (shiftState != ShiftState.OFF) palette.accentText else palette.keyText
                    Triple(sym, 18f, col)
                }
            }
            KeyType.BACKSPACE -> Triple("⌫", 18f, palette.keyText)
            KeyType.SYMBOLS -> Triple("?123", 13.5f, palette.keyText)
            KeyType.COMMA -> Triple(if (isChineseMode) "，" else ",", 18f, palette.keyText)
            KeyType.MODE -> Triple(if (isChineseMode) "中/En" else "En/中", 13f, palette.keyText)
            KeyType.SPACE -> Triple(if (isChineseMode) "空格" else "Space", 13.5f, palette.subText)
            KeyType.PERIOD -> Triple(if (isChineseMode) "。" else ".", 18f, palette.keyText)
            KeyType.ENTER -> Triple(enterLabel, 14.5f, palette.accentText)
        }

        val paint = if (k.type == KeyType.LETTER || k.type == KeyType.COMMA || k.type == KeyType.PERIOD) {
            textPaint
        } else {
            subTextPaint
        }
        paint.color = color
        paint.textSize = sizeSp * density
        val fm = paint.fontMetrics
        val baseline = cy - (fm.ascent + fm.descent) / 2f
        canvas.drawText(label, cx, baseline, paint)

        val alt = k.alt
        if (alt != null) {
            hintPaint.color = palette.subText
            hintPaint.textSize = 10.5f * density
            canvas.drawText(
                alt.toString(),
                k.rect.right - 4.5f * density,
                k.rect.top + 3f * density - hintPaint.fontMetrics.ascent,
                hintPaint
            )
        }
    }

    private fun drawPopup(canvas: Canvas, k: KeyDef, label: String, highlight: Boolean) {
        val popW = k.rect.width() * 1.32f
        val popH = k.rect.height() * 1.15f
        val left = (k.rect.centerX() - popW / 2f).coerceIn(2f * density, width - popW - 2f * density)
        val bottom = (k.rect.top - 4f * density).coerceAtLeast(popH + 2f * density)
        val top = bottom - popH

        popupRect.set(left, top, left + popW, bottom)
        popupPaint.color = palette.keyShadow
        tmpRect.set(popupRect.left, popupRect.top + 2f * density, popupRect.right, popupRect.bottom + 2f * density)
        canvas.drawRoundRect(tmpRect, 9f * density, 9f * density, popupPaint)

        popupPaint.color = if (highlight) palette.accentBg else palette.popupBg
        canvas.drawRoundRect(popupRect, 9f * density, 9f * density, popupPaint)

        textPaint.color = if (highlight) palette.accentText else palette.keyText
        textPaint.textSize = 26f * density
        val fm = textPaint.fontMetrics
        canvas.drawText(label, popupRect.centerX(), popupRect.centerY() - (fm.ascent + fm.descent) / 2f, textPaint)
    }

    private fun findKeyAt(x: Float, y: Float): KeyDef? {
        for (row in rows) {
            for (k in row) {
                if (k.rect.contains(x, y)) return k
            }
        }
        // Fallback to nearest key so edge taps between keys never get dropped.
        var best: KeyDef? = null
        var bestDist = Float.MAX_VALUE
        for (row in rows) {
            for (k in row) {
                val dx = (x - k.rect.centerX())
                val dy = (y - k.rect.centerY())
                val d = dx * dx + dy * dy
                if (d < bestDist) {
                    bestDist = d
                    best = k
                }
            }
        }
        return best
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> {
                val idx = event.actionIndex
                val pid = event.getPointerId(idx)
                val x = event.getX(idx)
                val y = event.getY(idx)
                val key = findKeyAt(x, y) ?: return true

                // Fast two-thumb typing: if another letter pointer is still down, commit it immediately.
                if (key.type == KeyType.LETTER && touches.isNotEmpty()) {
                    for ((oldPid, old) in touches.entries.toList()) {
                        if (old.key.type == KeyType.LETTER) {
                            touches.remove(oldPid)
                            cancelLongPress(oldPid)
                            commitTouch(old)
                        }
                    }
                }

                val touch = Touch(key, x, y)
                touches[pid] = touch
                if (hapticEnabled) {
                    performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
                }

                if (key.type == KeyType.BACKSPACE) {
                    listener?.onBackspace()
                    repeatingBackspace = true
                    handler.removeCallbacks(backspaceRepeater)
                    handler.postDelayed(backspaceRepeater, 360L)
                } else if (key.type == KeyType.LETTER && key.alt != null) {
                    scheduleLongPress(pid, touch)
                }
                invalidate()
                return true
            }

            MotionEvent.ACTION_MOVE -> {
                var changed = false
                for (i in 0 until event.pointerCount) {
                    val pid = event.getPointerId(i)
                    val t = touches[pid] ?: continue
                    if (t.key.type == KeyType.BACKSPACE || t.altFired) continue
                    val dx = event.getX(i) - t.downX
                    val dy = event.getY(i) - t.downY

                    // Swipe up on a letter key selects its quick symbol (committed on release).
                    val orig = t.origKey
                    if (orig.type == KeyType.LETTER && orig.alt != null &&
                        dy < -swipeThreshold && abs(dx) < orig.rect.width()
                    ) {
                        if (!t.swipeAlt) {
                            t.swipeAlt = true
                            t.key = orig
                            cancelLongPress(pid)
                            changed = true
                        }
                        continue
                    }
                    if (t.swipeAlt) {
                        if (dy < -swipeThreshold * 0.6f) continue // hysteresis
                        t.swipeAlt = false
                        changed = true
                    }

                    val next = findKeyAt(event.getX(i), event.getY(i))
                    if (next != null && next !== t.key) {
                        t.key = next
                        cancelLongPress(pid)
                        changed = true
                    }
                }
                if (changed) invalidate()
                return true
            }

            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> {
                val idx = event.actionIndex
                val pid = event.getPointerId(idx)
                cancelLongPress(pid)
                val t = touches.remove(pid)
                if (t != null) {
                    if (t.key.type == KeyType.BACKSPACE) {
                        stopBackspaceRepeat()
                    } else {
                        commitTouch(t)
                    }
                }
                invalidate()
                return true
            }

            MotionEvent.ACTION_CANCEL -> {
                touches.clear()
                for (r in longPressRunnables.values) handler.removeCallbacks(r)
                longPressRunnables.clear()
                stopBackspaceRepeat()
                invalidate()
                return true
            }
        }
        return super.onTouchEvent(event)
    }

    private fun commitTouch(t: Touch) {
        val alt = t.key.alt
        when {
            t.altFired -> Unit // already typed by long press
            t.swipeAlt && alt != null -> listener?.onQuickSymbol(alt)
            else -> fireKey(t.key)
        }
    }

    private fun scheduleLongPress(pid: Int, touch: Touch) {
        cancelLongPress(pid)
        val r = Runnable {
            longPressRunnables.remove(pid)
            val alt = touch.origKey.alt
            if (touches[pid] !== touch || touch.altFired || touch.swipeAlt ||
                touch.key !== touch.origKey || alt == null
            ) {
                return@Runnable
            }
            touch.altFired = true
            if (hapticEnabled) {
                performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
            }
            listener?.onQuickSymbol(alt)
            invalidate()
        }
        longPressRunnables[pid] = r
        handler.postDelayed(r, longPressMs)
    }

    private fun cancelLongPress(pid: Int) {
        longPressRunnables.remove(pid)?.let { handler.removeCallbacks(it) }
    }

    private fun stopBackspaceRepeat() {
        repeatingBackspace = false
        handler.removeCallbacks(backspaceRepeater)
    }

    private fun fireKey(k: KeyDef) {
        when (k.type) {
            KeyType.LETTER -> {
                val isUpper = (!isChineseMode || !isComposing) && shiftState != ShiftState.OFF
                listener?.onLetter(k.code, isUpper)
                if (shiftState == ShiftState.ONCE) {
                    shiftState = ShiftState.OFF
                }
            }
            KeyType.SHIFT_OR_SEP -> {
                if (isChineseMode && isComposing) {
                    listener?.onSeparator()
                } else {
                    val now = System.currentTimeMillis()
                    shiftState = when (shiftState) {
                        ShiftState.OFF -> ShiftState.ONCE
                        ShiftState.ONCE -> if (now - lastShiftTapMs < 350L) ShiftState.LOCKED else ShiftState.OFF
                        ShiftState.LOCKED -> ShiftState.OFF
                    }
                    lastShiftTapMs = now
                }
            }
            KeyType.BACKSPACE -> Unit // already fired on press
            KeyType.SYMBOLS -> listener?.onOpenSymbols()
            KeyType.COMMA -> listener?.onPunct(',')
            KeyType.MODE -> listener?.onToggleMode()
            KeyType.SPACE -> listener?.onSpace()
            KeyType.PERIOD -> listener?.onPunct('.')
            KeyType.ENTER -> listener?.onEnter()
        }
    }
}
