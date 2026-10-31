package {{application-id}}

import org.junit.Assert.assertTrue
import org.junit.Test

class ResourceCatalogTest {
    @Test fun catalogIdentifiersAreUnique() {
        assertTrue(resourceCatalog.map { it.id }.distinct().size == resourceCatalog.size)
    }
}
