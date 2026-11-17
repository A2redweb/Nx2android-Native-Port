package org.nx2a.nativeport

import android.view.View

interface Tab {
    val view: View
    /** the tab came to the front */
    fun onShow() {}
    /** called about once a second while it is in front */
    fun tick() {}
}
