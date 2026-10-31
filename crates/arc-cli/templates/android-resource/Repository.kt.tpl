package {{application-id}}.{{module}}

import android.content.Context
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.{{crate-name}}.NativeCore
import uniffi.{{crate-name}}.{{RecordType}}

interface {{Type}}Repository {
    suspend fun list(): List<{{RecordType}}>
    suspend fun get(id: String): {{RecordType}}
    suspend fun create(name: String): String
    suspend fun rename(id: String, name: String)
    suspend fun delete(id: String)
}

class Native{{Type}}Repository(context: Context) : {{Type}}Repository {
    private val application = context.applicationContext
    // Each call opens the lightweight wrapper; CoreRegistry retains the one typed
    // runtime/bus. No blocking initialization on the UI thread or automatic retry.
    private suspend fun core(): NativeCore = withContext(Dispatchers.IO) {
        val directory = application.filesDir
        check(directory.isDirectory || directory.mkdirs()) { "App storage is unavailable" }
        NativeCore.open(File(directory, "app.sqlite").canonicalPath)
    }
    override suspend fun list() = core().list{{pluralMethod}}()
    override suspend fun get(id: String) = core().get{{Type}}(id)
    override suspend fun create(name: String) = core().create{{Type}}(name)
    override suspend fun rename(id: String, name: String) = core().rename{{Type}}(id, name)
    override suspend fun delete(id: String) = core().delete{{Type}}(id)
}
