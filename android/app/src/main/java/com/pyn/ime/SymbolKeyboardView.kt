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

class SymbolKeyboardView(context: Context) : View(context) {

    enum class SymKeyType { TEXT, PAGE_TOGGLE, BACKSPACE, BACK_TO_ABC, SPACE, ENTER }

    data class SymKey(
        val type: SymKeyType,
        val label: String,
        val output: String = label,
        val weight: Float = 1f,
        val rect: RectF = RectF()
    )

    interface Listener {
        fun onSymbol(text: String)
        fun onBackspace()
        fun onSpace()
        fun onEnter()
        fun onBackToLetters()
    }

    var listener: Listener? = null
    var hapticEnabled: Boolean = true

    var isChineseMode: Boolean = true
        set(value) {
            if (field != value) {
                field = value
                rebuildLayout()
            }
        }

    var enterLabel: String = "换行"
        set(value) {
            if (field != value) {
                field = value
                invalidate()
            }
        }

    private var pageIndex: Int = 0
    private var palette = KeyboardTheme.current(context)
    private var rows: List<List<SymKey>> = emptyList()
    private var pressedKey: SymKey? = null

    private val density = resources.displayMetrics.density
    private val keyPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val shadowPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val textPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        textAlign = Paint.Align.CENTER
        typeface = Typeface.create("sans-serif", Typeface.NORMAL)
    }
    private val tmpRect = RectF()

    private val handler = Handler(Looper.getMainLooper())
    private var repeatingBackspace = false
    private val backspaceRepeater = object : Runnable {
        override fun run() {
            if (!repeatingBackspace) return
            listener?.onBackspace()
            handler.postDelayed(this, 55L)
        }
    }

    init {
        rebuildLayout()
    }

    fun refreshTheme() {
        palette = KeyboardTheme.current(context)
        invalidate()
    }

    fun resetPage() {
        pageIndex = 0
        rebuildLayout()
    }

    private fun buildRows(): List<List<SymKey>> {
        val r1 = listOf("1", "2", "3", "4", "5", "6", "7", "8", "9", "0")
            .map { SymKey(SymKeyType.TEXT, it) }

        val r2List = if (pageIndex == 0) {
            if (isChineseMode) {
                listOf("，", "。", "？", "！", "：", "；", "、", "“", "”", "-")
            } else {
                listOf("@", "#", "$", "_", "&", "-", "+", "(", ")", "/")
            }
        } else {
            listOf("~", "`", "|", "•", "√", "π", "÷", "×", "¥", "€")
        }
        val r2 = r2List.map { SymKey(SymKeyType.TEXT, it) }

        val r3List = if (pageIndex == 0) {
            if (isChineseMode) {
                listOf("（", "）", "《", "》", "【", "】", "……")
            } else {
                listOf("*", "\"", "'", ":", ";", "!", "?")
            }
        } else {
            listOf("^", "°", "=", "{", "}", "\\", "%")
        }
        val r3 = buildList {
            add(SymKey(SymKeyType.PAGE_TOGGLE, if (pageIndex == 0) "1/2" else "2/2", "", 1.5f))
            r3List.forEach { add(SymKey(SymKeyType.TEXT, it)) }
            add(SymKey(SymKeyType.BACKSPACE, "⌫", "", 1.5f))
        }

        val r4 = listOf(
            SymKey(SymKeyType.BACK_TO_ABC, "返回", "", 1.6f),
            SymKey(SymKeyType.TEXT, if (isChineseMode) "，" else ",", if (isChineseMode) "，" else ",", 1.0f),
            SymKey(SymKeyType.SPACE, "空格", " ", 4.4f),
            SymKey(SymKeyType.TEXT, if (isChineseMode) "。" else ".", if (isChineseMode) "。" else ".", 1.0f),
            SymKey(SymKeyType.ENTER, enterLabel, "\n", 1.8f)
        )
        return listOf(r1, r2, r3, r4)
    }

    private fun rebuildLayout() {
        rows = buildRows()
        if (width > 0 && height > 0) {
            layoutKeys(width.toFloat(), height.toFloat())
        }
        invalidate()
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
            val rowWidth = (width - 2f * padH).coerceAtLeast(1f)
            val usableW = rowWidth - gapX * (row.size - 1)
            var x = padH
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

        for (row in rows) {
            for (k in row) {
                val isPressed = k === pressedKey
                val bg = when (k.type) {
                    SymKeyType.TEXT, SymKeyType.SPACE -> if (isPressed) palette.keyPressedBg else palette.keyBg
                    SymKeyType.ENTER -> if (isPressed) palette.accentPressedBg else palette.accentBg
                    else -> if (isPressed) palette.funcKeyPressedBg else palette.funcKeyBg
                }
                shadowPaint.color = palette.keyShadow
                tmpRect.set(k.rect.left, k.rect.top + shadowOff, k.rect.right, k.rect.bottom + shadowOff)
                canvas.drawRoundRect(tmpRect, radius, radius, shadowPaint)

                keyPaint.color = bg
                canvas.drawRoundRect(k.rect, radius, radius, keyPaint)

                val label = if (k.type == SymKeyType.ENTER) enterLabel else k.label
                textPaint.color = when (k.type) {
                    SymKeyType.ENTER -> palette.accentText
                    SymKeyType.SPACE -> palette.subText
                    else -> palette.keyText
                }
                textPaint.textSize = when (k.type) {
                    SymKeyType.TEXT -> 18f * density
                    SymKeyType.BACKSPACE -> 18f * density
                    else -> 14f * density
                }
                val fm = textPaint.fontMetrics
                canvas.drawText(label, k.rect.centerX(), k.rect.centerY() - (fm.ascent + fm.descent) / 2f, textPaint)
            }
        }
    }

    private fun findKeyAt(x: Float, y: Float): SymKey? {
        for (row in rows) {
            for (k in row) {
                if (k.rect.contains(x, y)) return k
            }
        }
        return null
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                val k = findKeyAt(event.x, event.y) ?: return true
                pressedKey = k
                if (hapticEnabled) {
                    performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
                }
                if (k.type == SymKeyType.BACKSPACE) {
                    listener?.onBackspace()
                    repeatingBackspace = true
                    handler.removeCallbacks(backspaceRepeater)
                    handler.postDelayed(backspaceRepeater, 360L)
                }
                invalidate()
                return true
            }
            MotionEvent.ACTION_UP -> {
                val k = pressedKey
                pressedKey = null
                if (k != null) {
                    when (k.type) {
                        SymKeyType.TEXT -> listener?.onSymbol(k.output)
                        SymKeyType.PAGE_TOGGLE -> {
                            pageIndex = (pageIndex + 1) % 2
                            rebuildLayout()
                        }
                        SymKeyType.BACKSPACE -> stopBackspaceRepeat()
                        SymKeyType.BACK_TO_ABC -> listener?.onBackToLetters()
                        SymKeyType.SPACE -> listener?.onSpace()
                        SymKeyType.ENTER -> listener?.onEnter()
                    }
                }
                invalidate()
                return true
            }
            MotionEvent.ACTION_CANCEL -> {
                pressedKey = null
                stopBackspaceRepeat()
                invalidate()
                return true
            }
        }
        return super.onTouchEvent(event)
    }

    private fun stopBackspaceRepeat() {
        repeatingBackspace = false
        handler.removeCallbacks(backspaceRepeater)
    }
}
