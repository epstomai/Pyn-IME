package com.pyn.ime

import org.json.JSONObject

data class InitResult(
    val ok: Boolean,
    val loadMs: Float,
    val rerankerLoaded: Boolean,
    val error: String?
)

data class ActionResult(
    val action: String,
    val commitText: String?,
    val undoPrevious: Boolean,
    val composing: Boolean,
    val isChinese: Boolean,
    val inlinePreedit: String,
    val panelPreedit: String,
    val candidates: List<String>,
    val singleCharMode: Boolean,
    val latencyMs: Float
)

data class EngineStats(
    val ready: Boolean,
    val rerankerLoaded: Boolean,
    val useReranker: Boolean,
    val learningEnabled: Boolean,
    val csAiBoost: Boolean,
    val totalSteps: Long,
    val activeVocabWords: Int,
    val activeOovWords: Int,
    val stagedWords: Int,
    val activeBigrams: Int,
    val lastLatencyMs: Float
)

object NativeEngine {
    const val KEY_BACKSPACE = 0
    const val KEY_SPACE = 1
    const val KEY_ENTER = 2
    const val KEY_ESCAPE = 3
    const val KEY_APOSTROPHE = 4
    const val KEY_TOGGLE_SINGLE_CHAR = 5

    init {
        System.loadLibrary("pyn_ime")
    }

    private external fun nativeInit(
        modelPath: String,
        rerankerPath: String,
        userDictPath: String
    ): String

    private external fun nativeIsReady(): Boolean

    private external fun nativeSetSettings(
        useReranker: Boolean,
        learningEnabled: Boolean,
        csAiBoost: Boolean,
        chinesePunct: Boolean
    )

    private external fun nativeSetMode(isChinese: Boolean, contextBefore: String): String
    private external fun nativeHandleChar(
        codePoint: Int,
        isUpper: Boolean,
        contextBefore: String,
        decode: Boolean
    ): String

    private external fun nativeHandleKey(keyKind: Int, contextBefore: String): String
    private external fun nativePickCandidate(index: Int, contextBefore: String): String
    private external fun nativeCommitRaw(): String
    private external fun nativeCommitWithSuffix(suffix: String, contextBefore: String): String
    private external fun nativeResetSession()
    private external fun nativeSaveUserDict(): Boolean
    private external fun nativeStats(): String

    fun init(modelPath: String, rerankerPath: String, userDictPath: String): InitResult {
        val raw = nativeInit(modelPath, rerankerPath, userDictPath)
        val obj = JSONObject(raw)
        return InitResult(
            ok = obj.optBoolean("ok", false),
            loadMs = obj.optDouble("load_ms", 0.0).toFloat(),
            rerankerLoaded = obj.optBoolean("reranker_loaded", false),
            error = obj.optString("error", "").ifEmpty { null }
        )
    }

    fun isReady(): Boolean = nativeIsReady()

    fun setSettings(
        useReranker: Boolean,
        learningEnabled: Boolean,
        csAiBoost: Boolean,
        chinesePunct: Boolean
    ) {
        nativeSetSettings(useReranker, learningEnabled, csAiBoost, chinesePunct)
    }

    fun setMode(isChinese: Boolean, contextBefore: String): ActionResult =
        parseAction(nativeSetMode(isChinese, contextBefore))

    /** [decode] = false only extends the keys; the engine refreshes candidates before they are used. */
    fun handleChar(ch: Char, isUpper: Boolean, contextBefore: String, decode: Boolean = true): ActionResult =
        parseAction(nativeHandleChar(ch.code, isUpper, contextBefore, decode))

    fun handleKey(keyKind: Int, contextBefore: String): ActionResult =
        parseAction(nativeHandleKey(keyKind, contextBefore))

    fun pickCandidate(index: Int, contextBefore: String): ActionResult =
        parseAction(nativePickCandidate(index, contextBefore))

    fun commitRaw(): ActionResult = parseAction(nativeCommitRaw())

    fun commitWithSuffix(suffix: String, contextBefore: String): ActionResult =
        parseAction(nativeCommitWithSuffix(suffix, contextBefore))

    fun resetSession() = nativeResetSession()

    fun saveUserDict(): Boolean = nativeSaveUserDict()

    fun stats(): EngineStats {
        val obj = JSONObject(nativeStats())
        return EngineStats(
            ready = obj.optBoolean("ready", false),
            rerankerLoaded = obj.optBoolean("reranker_loaded", false),
            useReranker = obj.optBoolean("use_reranker", true),
            learningEnabled = obj.optBoolean("learning_enabled", true),
            csAiBoost = obj.optBoolean("cs_ai_boost", false),
            totalSteps = obj.optLong("total_steps", 0L),
            activeVocabWords = obj.optInt("active_vocab_words", 0),
            activeOovWords = obj.optInt("active_oov_words", 0),
            stagedWords = obj.optInt("staged_words", 0),
            activeBigrams = obj.optInt("active_bigrams", 0),
            lastLatencyMs = obj.optDouble("last_latency_ms", 0.0).toFloat()
        )
    }

    private fun parseAction(raw: String): ActionResult {
        val obj = JSONObject(raw)
        val arr = obj.optJSONArray("candidates")
        val cands = if (arr != null) {
            List(arr.length()) { i -> arr.optString(i, "") }
        } else {
            emptyList()
        }
        return ActionResult(
            action = obj.optString("action", "pass_through"),
            commitText = if (obj.has("commit_text") && !obj.isNull("commit_text")) {
                obj.optString("commit_text")
            } else {
                null
            },
            undoPrevious = obj.optBoolean("undo_previous", false),
            composing = obj.optBoolean("composing", false),
            isChinese = obj.optString("mode", "zh") == "zh",
            inlinePreedit = obj.optString("inline_preedit", ""),
            panelPreedit = obj.optString("panel_preedit", ""),
            candidates = cands,
            singleCharMode = obj.optBoolean("single_char_mode", false),
            latencyMs = obj.optDouble("latency_ms", 0.0).toFloat()
        )
    }
}
