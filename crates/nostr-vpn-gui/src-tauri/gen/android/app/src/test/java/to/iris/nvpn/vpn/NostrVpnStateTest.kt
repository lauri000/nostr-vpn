package to.iris.nvpn.vpn

import java.util.concurrent.ExecutionException
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class NostrVpnStateTest {
  @After
  fun tearDown() {
    NostrVpnState.clear()
  }

  @Test
  fun stopIntentIsSkippedWhenVpnServiceIsIdle() {
    assertFalse(NostrVpnState.shouldDispatchStopIntent())
  }

  @Test
  fun stopIntentIsDispatchedWhenTunnelIsActive() {
    NostrVpnState.active = true
    assertTrue(NostrVpnState.shouldDispatchStopIntent())
  }

  @Test
  fun cancelPendingStartCompletesFutureExceptionally() {
    val future =
      NostrVpnState.beginStart(
        TunnelConfig(
          sessionName = "mesh-home",
          localAddresses = listOf("10.44.180.104/32"),
          routes = listOf("10.44.199.77/32"),
          dnsServers = emptyList(),
          searchDomains = emptyList(),
          mtu = 1280,
        )
      )

    assertTrue(NostrVpnState.cancelPendingStart("Cancelled"))
    assertTrue(future.isCompletedExceptionally)
    assertEquals("Cancelled", NostrVpnState.lastError)

    try {
      future.get()
      fail("expected pending start cancellation to fail the future")
    } catch (error: ExecutionException) {
      assertEquals("Cancelled", error.cause?.message)
    }
  }
}
