package com.pyn.ime

import android.content.Context
import android.content.SharedPreferences

class Prefs(context: Context) {
    private val sp: SharedPreferences =
        context.applicationContext.getSharedPreferences("pyn_ime_prefs", Context.MODE_PRIVATE)

    var useReranker: Boolean
        get() = sp.getBoolean("use_reranker", true)
        set(value) = sp.edit().putBoolean("use_reranker", value).apply()

    var learningEnabled: Boolean
        get() = sp.getBoolean("learning_enabled", true)
        set(value) = sp.edit().putBoolean("learning_enabled", value).apply()

    var csAiBoost: Boolean
        get() = sp.getBoolean("cs_ai_boost", false)
        set(value) = sp.edit().putBoolean("cs_ai_boost", value).apply()

    var chinesePunct: Boolean
        get() = sp.getBoolean("chinese_punct", true)
        set(value) = sp.edit().putBoolean("chinese_punct", value).apply()

    var hapticEnabled: Boolean
        get() = sp.getBoolean("haptic_enabled", true)
        set(value) = sp.edit().putBoolean("haptic_enabled", value).apply()

    var keyPreviewEnabled: Boolean
        get() = sp.getBoolean("key_preview_enabled", true)
        set(value) = sp.edit().putBoolean("key_preview_enabled", value).apply()

    fun syncToEngine() {
        if (NativeEngine.isReady()) {
            NativeEngine.setSettings(
                useReranker = useReranker,
                learningEnabled = learningEnabled,
                csAiBoost = csAiBoost,
                chinesePunct = chinesePunct
            )
        }
    }
}
