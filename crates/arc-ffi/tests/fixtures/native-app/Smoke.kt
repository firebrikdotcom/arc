import kotlinx.coroutines.runBlocking
import uniffi.arc_ffi.FfiException
import uniffi.arc_ffi_native_app.NativeCore

fun main(args: Array<String>) = runBlocking {
    val core = NativeCore.open(args[0])
    check(core.createAndRead("jvm-smoke") == "visible")
    try {
        core.infrastructureError()
        error("external component error was not thrown")
    } catch (expected: FfiException.ProjectionRebuildFailed) {
        println("JVM native create/read and external FfiError passed")
    }
    core.close()
}
