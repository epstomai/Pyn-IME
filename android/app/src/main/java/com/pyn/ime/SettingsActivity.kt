package com.pyn.ime

import android.app.Activity
import android.content.Intent
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.os.Bundle
import android.provider.Settings
import android.util.TypedValue
import android.view.Gravity
import android.view.inputmethod.InputMethodManager
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.Switch
import android.widget.TextView

class SettingsActivity : Activity() {

    private lateinit var prefs: Prefs
    private lateinit var statusTv: TextView
    private lateinit var statsTv: TextView

    private val modelListener: (ModelManager.State) -> Unit = { st ->
        renderStatus(st)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        title = "Pyn 拼音 (26键)"
        prefs = Prefs(this)

        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val palette = KeyboardTheme.current(this)

        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(16), dp(16), dp(16), dp(24))
        }

        // Status Card
        val card = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(14), dp(12), dp(14), dp(12))
            background = GradientDrawable().apply {
                setColor(palette.barBg)
                cornerRadius = dp(10).toFloat()
            }
        }
        statusTv = TextView(this).apply {
            text = "引擎状态：初始化中…"
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            setTextColor(palette.keyText)
        }
        statsTv = TextView(this).apply {
            text = ""
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12.5f)
            setTextColor(palette.subText)
            setPadding(0, dp(4), 0, 0)
        }
        card.addView(statusTv)
        card.addView(statsTv)
        root.addView(card, LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT,
            LinearLayout.LayoutParams.WRAP_CONTENT
        ).apply { bottomMargin = dp(14) })

        // Setup buttons
        val btnEnable = Button(this).apply {
            text = "1. 在系统设置中启用「Pyn 拼音」"
            setOnClickListener {
                startActivity(Intent(Settings.ACTION_INPUT_METHOD_SETTINGS))
            }
        }
        val btnSwitch = Button(this).apply {
            text = "2. 切换输入法为「Pyn 拼音」"
            setOnClickListener {
                val imm = getSystemService(INPUT_METHOD_SERVICE) as InputMethodManager
                imm.showInputMethodPicker()
            }
        }
        root.addView(btnEnable, LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT
        ).apply { bottomMargin = dp(8) })
        root.addView(btnSwitch, LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT
        ).apply { bottomMargin = dp(14) })

        // Test input box
        val testLabel = TextView(this).apply {
            text = "打字测试区"
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 13f)
            setTextColor(palette.subText)
        }
        val testInput = EditText(this).apply {
            hint = "点击这里弹出 Pyn 拼音测试输入…"
            minLines = 3
            gravity = Gravity.TOP or Gravity.START
        }
        root.addView(testLabel)
        root.addView(testInput, LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT
        ).apply { bottomMargin = dp(16) })

        // Switches
        fun addSwitch(label: String, initial: Boolean, onToggle: (Boolean) -> Unit) {
            val row = LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(dp(4), dp(8), dp(4), dp(8))
            }
            val tv = TextView(this).apply {
                text = label
                setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
                setTextColor(palette.keyText)
            }
            val sw = Switch(this).apply {
                isChecked = initial
                setOnCheckedChangeListener { _, checked ->
                    onToggle(checked)
                    prefs.syncToEngine()
                    renderStatus(ModelManager.state)
                }
            }
            row.addView(tv, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
            row.addView(sw)
            root.addView(row)
        }

        addSwitch("神经网络重排（Reranker）", prefs.useReranker) { prefs.useReranker = it }
        addSwitch("用户词库自学习", prefs.learningEnabled) { prefs.learningEnabled = it }
        addSwitch("CS / AI 术语增强", prefs.csAiBoost) { prefs.csAiBoost = it }
        addSwitch("默认中文标点", prefs.chinesePunct) { prefs.chinesePunct = it }
        addSwitch("按键震动反馈", prefs.hapticEnabled) { prefs.hapticEnabled = it }
        addSwitch("按键气泡预览", prefs.keyPreviewEnabled) { prefs.keyPreviewEnabled = it }

        setContentView(ScrollView(this).apply { addView(root) })

        ModelManager.addListener(modelListener)
        ModelManager.ensureInitialized(this)
    }

    override fun onResume() {
        super.onResume()
        renderStatus(ModelManager.state)
    }

    override fun onDestroy() {
        ModelManager.removeListener(modelListener)
        super.onDestroy()
    }

    private fun renderStatus(st: ModelManager.State) {
        when (st) {
            is ModelManager.State.Idle -> {
                statusTv.text = "引擎状态：等待初始化"
                statsTv.text = ""
            }
            is ModelManager.State.Loading -> {
                statusTv.text = "引擎状态：${st.stage}"
                statsTv.text = "首次启动需解压内置模型（~179 MB），请稍候…"
            }
            is ModelManager.State.Ready -> {
                statusTv.text = "引擎状态：已就绪（加载耗时 %.0f ms）".format(st.loadMs)
                val info = NativeEngine.stats()
                statsTv.text = "Reranker: %s | 词汇学习: %d 词 / %d 新词 | 最近解码: %.1f ms".format(
                    if (info.rerankerLoaded && info.useReranker) "开启" else "关闭",
                    info.activeVocabWords,
                    info.activeOovWords,
                    info.lastLatencyMs
                )
            }
            is ModelManager.State.Error -> {
                statusTv.text = "引擎状态：加载失败"
                statsTv.text = st.message
            }
        }
    }
}
