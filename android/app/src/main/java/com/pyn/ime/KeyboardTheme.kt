package com.pyn.ime

import android.content.Context
import android.content.res.Configuration
import android.graphics.Color

data class KeyboardPalette(
    val isDark: Boolean,
    val bg: Int,
    val barBg: Int,
    val keyBg: Int,
    val keyPressedBg: Int,
    val funcKeyBg: Int,
    val funcKeyPressedBg: Int,
    val accentBg: Int,
    val accentPressedBg: Int,
    val keyShadow: Int,
    val keyText: Int,
    val subText: Int,
    val accentText: Int,
    val divider: Int,
    val chipBg: Int,
    val chipActiveBg: Int,
    val popupBg: Int
)

object KeyboardTheme {
    fun current(context: Context): KeyboardPalette {
        val night = (context.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) ==
            Configuration.UI_MODE_NIGHT_YES
        return if (night) {
            KeyboardPalette(
                isDark = true,
                bg = Color.parseColor("#181A1F"),
                barBg = Color.parseColor("#1F2228"),
                keyBg = Color.parseColor("#2E323B"),
                keyPressedBg = Color.parseColor("#444B58"),
                funcKeyBg = Color.parseColor("#242830"),
                funcKeyPressedBg = Color.parseColor("#39404D"),
                accentBg = Color.parseColor("#3B82F6"),
                accentPressedBg = Color.parseColor("#2563EB"),
                keyShadow = Color.parseColor("#0D0F12"),
                keyText = Color.parseColor("#F3F4F6"),
                subText = Color.parseColor("#9CA3AF"),
                accentText = Color.WHITE,
                divider = Color.parseColor("#2D323C"),
                chipBg = Color.parseColor("#2A2F38"),
                chipActiveBg = Color.parseColor("#1D4ED8"),
                popupBg = Color.parseColor("#3F4654")
            )
        } else {
            KeyboardPalette(
                isDark = false,
                bg = Color.parseColor("#E5E7EC"),
                barBg = Color.parseColor("#F1F3F6"),
                keyBg = Color.parseColor("#FFFFFF"),
                keyPressedBg = Color.parseColor("#D5DCE8"),
                funcKeyBg = Color.parseColor("#CFD4DF"),
                funcKeyPressedBg = Color.parseColor("#BAC2D1"),
                accentBg = Color.parseColor("#2563EB"),
                accentPressedBg = Color.parseColor("#1D4ED8"),
                keyShadow = Color.parseColor("#B5BCC9"),
                keyText = Color.parseColor("#111827"),
                subText = Color.parseColor("#6B7280"),
                accentText = Color.WHITE,
                divider = Color.parseColor("#D1D5DB"),
                chipBg = Color.parseColor("#E2E6EE"),
                chipActiveBg = Color.parseColor("#DBEAFE"),
                popupBg = Color.parseColor("#FFFFFF")
            )
        }
    }
}
