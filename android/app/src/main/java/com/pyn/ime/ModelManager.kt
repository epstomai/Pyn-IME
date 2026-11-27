package com.pyn.ime

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.util.Log
import java.io.File
import java.io.FileOutputStream
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.Executors

object ModelManager {
    private const val TAG = "PynModelManager"
    private const val MODEL_NAME = "ime_model.bin"
    private const val RERANKER_NAME = "reranker.bin"
    private const val USER_DICT_NAME = "user_dict.tsv"

    sealed class State {
        data object Idle : State()
        data class Loading(val stage: String) : State()
        data class Ready(val loadMs: Float, val rerankerLoaded: Boolean) : State()
        data class Error(val message: String) : State()
    }

    @Volatile
    var state: State = State.Idle
        private set

    private val mainHandler = Handler(Looper.getMainLooper())
    private val listeners = CopyOnWriteArrayList<(State) -> Unit>()
    private val executor = Executors.newSingleThreadExecutor { r ->
        Thread(r, "pyn-engine-loader").apply { isDaemon = true }
    }

    @Volatile
    private var started = false

    fun addListener(listener: (State) -> Unit) {
        listeners.add(listener)
        mainHandler.post { listener(state) }
    }

    fun removeListener(listener: (State) -> Unit) {
        listeners.remove(listener)
    }

    private fun updateState(newState: State) {
        state = newState
        mainHandler.post {
            for (l in listeners) {
                l(newState)
            }
        }
    }

    fun ensureInitialized(context: Context) {
        val appCtx = context.applicationContext
        // Already loaded: skip JNI (it would wait on the engine lock during a decode).
        if (state is State.Ready) return
        if (NativeEngine.isReady()) {
            if (state !is State.Ready) {
                val st = NativeEngine.stats()
                updateState(State.Ready(0f, st.rerankerLoaded))
            }
            return
        }
        synchronized(this) {
            if (started) return
            started = true
        }
        updateState(State.Loading("正在准备词库…"))
        executor.execute {
            try {
                val modelsDir = File(appCtx.filesDir, "models").apply { mkdirs() }
                val modelFile = extractAssetIfNeeded(appCtx, "models/$MODEL_NAME", File(modelsDir, MODEL_NAME))
                    ?: throw IllegalStateException("未找到内置模型 $MODEL_NAME")
                val rerankerFile = extractAssetIfNeeded(appCtx, "models/$RERANKER_NAME", File(modelsDir, RERANKER_NAME))
                val userDictFile = File(appCtx.filesDir, USER_DICT_NAME)

                updateState(State.Loading("正在加载词库模型…"))
                val res = NativeEngine.init(
                    modelPath = modelFile.absolutePath,
                    rerankerPath = rerankerFile?.absolutePath ?: "",
                    userDictPath = userDictFile.absolutePath
                )
                if (res.ok) {
                    Prefs(appCtx).syncToEngine()
                    Log.i(TAG, "Pyn engine ready in ${res.loadMs}ms (reranker=${res.rerankerLoaded})")
                    updateState(State.Ready(res.loadMs, res.rerankerLoaded))
                } else {
                    synchronized(this) { started = false }
                    val err = res.error ?: "未知加载错误"
                    Log.e(TAG, "Pyn engine init failed: $err")
                    updateState(State.Error(err))
                }
            } catch (t: Throwable) {
                synchronized(this) { started = false }
                Log.e(TAG, "ModelManager failure", t)
                updateState(State.Error(t.message ?: t.javaClass.simpleName))
            }
        }
    }

    private fun extractAssetIfNeeded(context: Context, assetPath: String, destFile: File): File? {
        val expectedSize = try {
            context.assets.openFd(assetPath).use { it.length }
        } catch (_: Throwable) {
            -1L
        }
        if (destFile.isFile && expectedSize > 0 && destFile.length() == expectedSize) {
            return destFile
        }
        val input = try {
            context.assets.open(assetPath)
        } catch (_: Throwable) {
            return null
        }
        val tmp = File(destFile.parentFile, "${destFile.name}.tmp")
        input.use { src ->
            FileOutputStream(tmp).use { dst ->
                val buf = ByteArray(256 * 1024)
                while (true) {
                    val n = src.read(buf)
                    if (n <= 0) break
                    dst.write(buf, 0, n)
                }
                dst.fd.sync()
            }
        }
        if (destFile.exists()) {
            destFile.delete()
        }
        if (!tmp.renameTo(destFile)) {
            throw IllegalStateException("无法写入 ${destFile.name}")
        }
        return destFile
    }

    fun saveUserDictAsync() {
        executor.execute {
            if (NativeEngine.isReady()) {
                NativeEngine.saveUserDict()
            }
        }
    }
}
