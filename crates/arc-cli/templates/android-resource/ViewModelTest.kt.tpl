package {{application-id}}.{{module}}

import androidx.lifecycle.SavedStateHandle
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.*
import org.junit.After
import org.junit.Before
import org.junit.Test
import org.junit.Assert.*
import uniffi.{{crate-name}}.{{RecordType}}

@OptIn(ExperimentalCoroutinesApi::class)
class {{Type}}ViewModelTest {
    private val dispatcher = StandardTestDispatcher()
    @Before fun setup() { Dispatchers.setMain(dispatcher) }
    @After fun teardown() { Dispatchers.resetMain() }

    private class Fake : {{Type}}Repository {
        val records = mutableListOf<{{RecordType}}>()
        var creates = 0
        var failRead = false
        var cancelWrite = false
        override suspend fun list(): List<{{RecordType}}> {
            check(!failRead) { "Read unavailable" }
            return records.toList()
        }
        override suspend fun get(id: String) = records.first { it.id == id }
        override suspend fun create(name: String): String {
            creates++
            if (cancelWrite) throw CancellationException("cancelled")
            require(name.isNotBlank()) { "name cannot be empty" }
            val id = "id-$creates"
            records += {{RecordType}}(id, name, 1L)
            return id
        }
        override suspend fun rename(id: String, name: String) {
            require(name.isNotBlank()) { "name cannot be empty" }
            val index = records.indexOfFirst { it.id == id }
            records[index] = {{RecordType}}(id, name, records[index].version + 1)
        }
        override suspend fun delete(id: String) { records.removeAll { it.id == id } }
    }
    @Test fun crudRefreshesListAndDetailAndRetainsInvalidEdits() = runTest(dispatcher) {
        val fake = Fake()
        val model = {{Type}}ViewModel(fake, SavedStateHandle())
        assertTrue(model.state.value.busy)
        advanceUntilIdle()
        model.create()
        model.editDraft(" ")
        model.save()
        advanceUntilIdle()
        assertEquals(" ", model.state.value.draft)
        assertEquals("create", model.state.value.mode)
        assertNotNull(model.state.value.error)
        model.editDraft("First")
        model.save()
        model.save() // Busy writes must not submit twice.
        advanceUntilIdle()
        assertEquals(2, fake.creates) // one invalid attempt, one successful attempt
        assertEquals("Saved", model.state.value.notice)
        assertEquals("First", model.state.value.selected?.name)
        assertEquals(1, model.state.value.records.size)
        model.edit()
        model.editDraft("Renamed")
        model.save()
        advanceUntilIdle()
        assertEquals("Renamed", model.state.value.records.single().name)
        assertEquals(2L, model.state.value.selected?.version)
        model.delete()
        advanceUntilIdle()
        assertEquals("list", model.state.value.mode)
        assertTrue(model.state.value.records.isEmpty())
    }
    @Test fun restorationReadFailureAndCancellationNeverRetryWrites() = runTest(dispatcher) {
        val fake = Fake()
        val saved = SavedStateHandle()
        val model = {{Type}}ViewModel(fake, saved)
        advanceUntilIdle()
        model.create()
        model.editDraft("Restored draft")
        val restored = {{Type}}ViewModel(fake, saved)
        advanceUntilIdle()
        assertEquals("Restored draft", restored.state.value.draft)
        assertEquals("create", restored.state.value.mode)
        fake.cancelWrite = true
        restored.save()
        advanceUntilIdle()
        assertEquals(1, fake.creates)
        assertNull(restored.state.value.error)
        assertFalse(restored.state.value.busy)
        fake.cancelWrite = false
        fake.failRead = true
        restored.save()
        advanceUntilIdle()
        assertEquals(2, fake.creates)
        assertEquals("detail", restored.state.value.mode)
        assertEquals("Saved", restored.state.value.notice)
        assertEquals("Read unavailable", restored.state.value.error)
        fake.failRead = false
        restored.refresh()
        advanceUntilIdle()
        assertEquals(2, fake.creates)
        assertEquals("Restored draft", restored.state.value.selected?.name)
    }
}
