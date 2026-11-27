package com.pyn.ime

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RectF
import android.graphics.Typeface
import android.content.res.ColorStateList
import android.graphics.drawable.GradientDrawable
import android.graphics.drawable.StateListDrawable
import android.text.TextUtils
import android.util.TypedValue
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.ViewConfiguration
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import kotlin.math.abs

/**
 * Custom Canvas horizontal candidate strip with smooth drag scrolling and zero View-tree thrashing.
 */
class CandidateStripView(context: Context) : View(context) {

    var onPick: ((Int) -> Unit)? = null

    private val density = resources.displayMetrics.density
    private val touchSlop = ViewConfiguration.get(context).scaledTouchSlop.toFloat()
    private var palette = KeyboardTheme.current(context)

    private var items: List<String> = emptyList()
    private val itemStarts = ArrayList<Float>()
    private val itemEnds = ArrayList<Float>()
    private var totalContentWidth = 0f
    private var scrollXOffset = 0f

    private var downX = 0f
    private var lastX = 0f
    private var dragging = false
    private var pressedIndex = -1

    private val textPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        textAlign = Paint.Align.CENTER
        textSize = 18f * density
        typeface = Typeface.create("sans-serif", Typeface.NORMAL)
    }
    private val firstTextPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        textAlign = Paint.Align.CENTER
        textSize = 18f * density
        typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
    }
    private val dividerPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        strokeWidth = 1f * density
    }
    private val pressedPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val tmpRect = RectF()

    fun refreshTheme() {
        palette = KeyboardTheme.current(context)
        invalidate()
    }

    fun setCandidates(candidates: List<String>) {
        items = if (candidates.size > 60) candidates.subList(0, 60) else candidates
        scrollXOffset = 0f
        pressedIndex = -1
        dragging = false
        measureItems()
        invalidate()
    }

    /** Number of leading candidates fully visible in the strip at its current scroll position. */
    fun fullyVisibleCount(): Int {
        if (width <= 0) return 0
        val right = scrollXOffset + width.toFloat()
        var n = 0
        while (n < items.size && itemEnds[n] <= right + 0.5f) n++
        return n
    }

    private fun measureItems() {
        itemStarts.clear()
        itemEnds.clear()
        val padX = 14f * density
        val minW = 42f * density
        var x = 6f * density
        for ((i, text) in items.withIndex()) {
            val p = if (i == 0) firstTextPaint else textPaint
            val w = (p.measureText(text) + padX * 2f).coerceAtLeast(minW)
            itemStarts.add(x)
            itemEnds.add(x + w)
            x += w
        }
        totalContentWidth = x + 6f * density
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        val w = MeasureSpec.getSize(widthMeasureSpec)
        val h = resolveSize((44f * density).toInt(), heightMeasureSpec)
        setMeasuredDimension(w, h)
    }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        if (items.isEmpty()) return

        val h = height.toFloat()
        val cy = h / 2f
        val maxScroll = (totalContentWidth - width.toFloat()).coerceAtLeast(0f)
        scrollXOffset = scrollXOffset.coerceIn(0f, maxScroll)

        canvas.save()
        canvas.translate(-scrollXOffset, 0f)

        val visLeft = scrollXOffset - 16f
        val visRight = scrollXOffset + width.toFloat() + 16f
        dividerPaint.color = palette.divider

        for (i in items.indices) {
            val l = itemStarts[i]
            val r = itemEnds[i]
            if (r < visLeft) continue
            if (l > visRight) break

            if (i == pressedIndex) {
                pressedPaint.color = palette.chipBg
                tmpRect.set(l + 2f * density, 4f * density, r - 2f * density, h - 4f * density)
                canvas.drawRoundRect(tmpRect, 6f * density, 6f * density, pressedPaint)
            }

            val p = if (i == 0) firstTextPaint else textPaint
            p.color = if (i == 0) palette.accentBg else palette.keyText
            val fm = p.fontMetrics
            val baseline = cy - (fm.ascent + fm.descent) / 2f
            canvas.drawText(items[i], (l + r) / 2f, baseline, p)

            if (i + 1 < items.size) {
                canvas.drawLine(r, h * 0.25f, r, h * 0.75f, dividerPaint)
            }
        }
        canvas.restore()
    }

    private fun hitIndex(viewX: Float): Int {
        val contentX = viewX + scrollXOffset
        for (i in items.indices) {
            if (contentX >= itemStarts[i] && contentX < itemEnds[i]) {
                return i
            }
        }
        return -1
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (items.isEmpty()) return false
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                downX = event.x
                lastX = event.x
                dragging = false
                pressedIndex = hitIndex(event.x)
                invalidate()
                return true
            }
            MotionEvent.ACTION_MOVE -> {
                val dxTotal = event.x - downX
                if (!dragging && abs(dxTotal) > touchSlop) {
                    dragging = true
                    pressedIndex = -1
                }
                if (dragging) {
                    val dx = lastX - event.x
                    val maxScroll = (totalContentWidth - width.toFloat()).coerceAtLeast(0f)
                    scrollXOffset = (scrollXOffset + dx).coerceIn(0f, maxScroll)
                    invalidate()
                }
                lastX = event.x
                return true
            }
            MotionEvent.ACTION_UP -> {
                val idx = pressedIndex
                val wasDrag = dragging
                pressedIndex = -1
                dragging = false
                invalidate()
                if (!wasDrag && idx >= 0 && idx < items.size) {
                    onPick?.invoke(idx)
                }
                return true
            }
            MotionEvent.ACTION_CANCEL -> {
                pressedIndex = -1
                dragging = false
                invalidate()
                return true
            }
        }
        return super.onTouchEvent(event)
    }
}

/**
 * The IME root uses clipChildren=false (key previews draw outside their key), so nothing clips this
 * panel; without its own clip, scrolled-out grid rows would be painted over the candidate bar.
 */
class ClippedScrollView(context: Context) : ScrollView(context) {
    override fun draw(canvas: Canvas) {
        canvas.save()
        canvas.clipRect(scrollX, scrollY, scrollX + width, scrollY + height)
        super.draw(canvas)
        canvas.restore()
    }
}

class CandidateBarView(context: Context) : LinearLayout(context) {

    interface Listener {
        fun onPickCandidate(index: Int)
        fun onToggleExpand(expanded: Boolean)
        fun onToggleSingleChar()
        fun onOpenSettings()
    }

    var listener: Listener? = null

    private val density = resources.displayMetrics.density
    private var palette = KeyboardTheme.current(context)
    private var expanded = false
    /** First candidate shown in the expanded grid: the ones visible in the strip are not repeated. */
    private var gridStart = 0
    private var currentCandidates: List<String> = emptyList()
    private var statusText: String = ""

    // Fixed-height 2-row header (22dp preedit/status + 44dp candidates/toolbar) so IME window height never jumps.
    private val topStrip: LinearLayout
    private val preeditLabel: TextView
    private val singleCharBtn: TextView
    private val mainRow: LinearLayout
    private val idleToolbar: LinearLayout
    private val statusBadge: TextView
    private val settingsBtn: ImageView
    private val candidateStrip: CandidateStripView
    private val expandBtn: TextView
    private val dividerView: View

    val expandedPanel: ScrollView
    private val expandedGrid: LinearLayout

    init {
        orientation = VERTICAL
        setBackgroundColor(palette.barBg)

        // Row 1 (fixed 22dp): preedit / syllable segmentation & single-char jump button
        topStrip = LinearLayout(context).apply {
            orientation = HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(12), dp(2), dp(8), 0)
        }
        preeditLabel = TextView(context).apply {
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12.5f)
            setTextColor(palette.subText)
            maxLines = 1
            ellipsize = TextUtils.TruncateAt.START
            text = ""
        }
        singleCharBtn = TextView(context).apply {
            text = "单字"
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 11f)
            setPadding(dp(8), dp(1), dp(8), dp(1))
            gravity = Gravity.CENTER
            visibility = INVISIBLE
            setOnClickListener { listener?.onToggleSingleChar() }
        }
        topStrip.addView(preeditLabel, LayoutParams(0, LayoutParams.WRAP_CONTENT, 1f))
        topStrip.addView(singleCharBtn, LayoutParams(LayoutParams.WRAP_CONTENT, LayoutParams.WRAP_CONTENT))
        addView(topStrip, LayoutParams(LayoutParams.MATCH_PARENT, dp(22)))

        // Row 2 (fixed 44dp): either idle toolbar or Canvas horizontal candidate strip
        mainRow = LinearLayout(context).apply {
            orientation = HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
        }

        idleToolbar = LinearLayout(context).apply {
            orientation = HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(12), 0, dp(4), 0)
        }
        statusBadge = TextView(context).apply {
            text = statusText
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 13.5f)
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            setTextColor(palette.subText)
        }
        settingsBtn = ImageView(context).apply {
            setImageResource(R.drawable.ic_settings)
            scaleType = ImageView.ScaleType.CENTER_INSIDE
            setPadding(dp(9), dp(9), dp(9), dp(9))
            contentDescription = "设置"
            isClickable = true
            setOnClickListener { listener?.onOpenSettings() }
        }
        // Idle toolbar: status text (only while loading / on error), settings key at the far right.
        idleToolbar.addView(statusBadge, LayoutParams(0, LayoutParams.WRAP_CONTENT, 1f))
        idleToolbar.addView(settingsBtn, LayoutParams(dp(40), dp(40)))

        candidateStrip = CandidateStripView(context).apply {
            visibility = GONE
            onPick = { idx -> listener?.onPickCandidate(idx) }
        }

        expandBtn = TextView(context).apply {
            text = "∨"
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
            gravity = Gravity.CENTER
            setTextColor(palette.subText)
            visibility = GONE
            setOnClickListener { setExpanded(!expanded) }
        }

        mainRow.addView(idleToolbar, LayoutParams(LayoutParams.MATCH_PARENT, dp(44)))
        mainRow.addView(candidateStrip, LayoutParams(0, dp(44), 1f))
        mainRow.addView(expandBtn, LayoutParams(dp(42), dp(44)))
        addView(mainRow, LayoutParams(LayoutParams.MATCH_PARENT, dp(44)))

        dividerView = View(context).apply {
            setBackgroundColor(palette.divider)
        }
        addView(dividerView, LayoutParams(LayoutParams.MATCH_PARENT, 1))

        // Expanded candidate grid (attached to keyboard container by PynImeService)
        expandedGrid = LinearLayout(context).apply {
            orientation = VERTICAL
            setPadding(dp(6), dp(6), dp(6), dp(10))
        }
        expandedPanel = ClippedScrollView(context).apply {
            setBackgroundColor(palette.bg)
            visibility = GONE
            addView(expandedGrid, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.WRAP_CONTENT))
        }

        applyThemeColors()
    }


    fun refreshTheme() {
        palette = KeyboardTheme.current(context)
        applyThemeColors()
        candidateStrip.refreshTheme()
        if (expanded) rebuildExpandedGrid()
    }

    private fun applyThemeColors() {
        setBackgroundColor(palette.barBg)
        preeditLabel.setTextColor(palette.subText)
        statusBadge.setTextColor(palette.subText)
        expandBtn.setTextColor(palette.subText)
        dividerView.setBackgroundColor(palette.divider)
        expandedPanel.setBackgroundColor(palette.bg)

        val btnBg = GradientDrawable().apply {
            setColor(palette.chipBg)
            cornerRadius = dp(6).toFloat()
        }
        settingsBtn.background = StateListDrawable().apply {
            addState(intArrayOf(android.R.attr.state_pressed), btnBg)
        }
        settingsBtn.imageTintList = ColorStateList.valueOf(palette.subText)
    }

    fun setStatus(text: String) {
        statusText = text
        statusBadge.text = statusText
    }

    fun setCandidates(
        composing: Boolean,
        panelPreedit: String,
        candidates: List<String>,
        singleCharMode: Boolean
    ) {
        currentCandidates = candidates
        if (!composing && candidates.isEmpty()) {
            preeditLabel.text = ""
            singleCharBtn.visibility = INVISIBLE
            idleToolbar.visibility = VISIBLE
            candidateStrip.visibility = GONE
            candidateStrip.setCandidates(emptyList())
            expandBtn.visibility = GONE
            setExpanded(false)
            return
        }

        preeditLabel.text = panelPreedit
        val singleBg = GradientDrawable().apply {
            setColor(if (singleCharMode) palette.accentBg else palette.chipBg)
            cornerRadius = dp(5).toFloat()
        }
        singleCharBtn.background = singleBg
        singleCharBtn.setTextColor(if (singleCharMode) palette.accentText else palette.subText)
        singleCharBtn.visibility = if (panelPreedit.contains('\'') || panelPreedit.length >= 2) VISIBLE else INVISIBLE

        idleToolbar.visibility = GONE
        candidateStrip.visibility = VISIBLE
        candidateStrip.setCandidates(candidates)
        expandBtn.visibility = if (expanded || candidates.size > 4) VISIBLE else GONE
        if (expanded) {
            rebuildExpandedGrid()
        }
    }

    fun collapseIfExpanded() = setExpanded(false)

    /**
     * Expanded: the candidate row and its expand key stay as they are (arrow flips to ∧); the grid
     * below continues with the candidates the row does not fully show, so nothing appears twice.
     * Picking a candidate keeps the panel open while input remains; it closes by itself once
     * everything is committed.
     */
    private fun setExpanded(value: Boolean) {
        if (expanded == value) return
        expanded = value
        expandBtn.text = if (value) "∧" else "∨"
        if (value) {
            expandBtn.visibility = VISIBLE
            rebuildExpandedGrid()
        }
        listener?.onToggleExpand(value)
    }

    private fun rebuildExpandedGrid() {
        expandedGrid.removeAllViews()
        val cols = 5
        gridStart = candidateStrip.fullyVisibleCount().coerceAtMost(currentCandidates.size)
        val limit = currentCandidates.size.coerceAtMost(gridStart + 100)
        var i = gridStart
        while (i < limit) {
            val row = LinearLayout(context).apply {
                orientation = HORIZONTAL
            }
            for (c in 0 until cols) {
                val idx = i + c
                val cell = TextView(context).apply {
                    gravity = Gravity.CENTER
                    setTextSize(TypedValue.COMPLEX_UNIT_SP, 17f)
                    maxLines = 1
                    ellipsize = TextUtils.TruncateAt.END
                    if (idx < limit) {
                        text = currentCandidates[idx]
                        setTextColor(if (idx == 0) palette.accentBg else palette.keyText)
                        background = GradientDrawable().apply {
                            setColor(palette.keyBg)
                            cornerRadius = dp(6).toFloat()
                        }
                        setOnClickListener { listener?.onPickCandidate(idx) }
                    }
                }
                row.addView(cell, LayoutParams(0, dp(42), 1f).apply {
                    setMargins(dp(3), dp(3), dp(3), dp(3))
                })
            }
            expandedGrid.addView(row, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.WRAP_CONTENT))
            i += cols
        }
        expandedPanel.scrollTo(0, 0)
    }

    private fun dp(v: Int): Int = (v * density).toInt()
}
