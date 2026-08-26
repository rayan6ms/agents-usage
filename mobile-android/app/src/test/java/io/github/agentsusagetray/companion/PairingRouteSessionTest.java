package io.github.agentsusagetray.companion;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import java.util.Collections;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;
import org.junit.Test;

public class PairingRouteSessionTest {
    private static final String TOKEN =
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    private static final String LAN = "http://192.168.1.20:3765/pair?token=" + TOKEN;
    private static final String TAILSCALE =
            "https://desktop.example.ts.net/agents-usage/pair?token=" + TOKEN;

    @Test
    public void oneFailedRouteDoesNotDiscardAnotherSuccessfulRoute() {
        PairingRouteSession session = session(Collections.emptySet());
        assertEquals("http://192.168.1.20:3765/", session.next().baseUrl);
        session.succeedCurrent();
        assertEquals("https://desktop.example.ts.net/agents-usage/", session.next().baseUrl);
        session.failCurrent("Tailscale is not connected.");

        assertNull(session.next());
        assertEquals(1, session.successCount());
        assertTrue(session.hasFailures());
        assertTrue(session.failureSummary().contains("Tailscale is not connected"));
    }

    @Test
    public void retrySkipsAHealthySavedRouteWithoutReusingItsToken() {
        Set<String> healthy = new LinkedHashSet<>();
        healthy.add("http://192.168.1.20:3765/");
        PairingRouteSession session = session(healthy);

        assertEquals(1, session.successCount());
        assertEquals("https://desktop.example.ts.net/agents-usage/", session.next().baseUrl);
        session.succeedCurrent();
        assertNull(session.next());
        assertEquals(2, session.successCount());
        assertFalse(session.hasFailures());
    }

    @Test
    public void allFailedRoutesRemainAnOverallFailure() {
        PairingRouteSession session = session(Collections.emptySet());
        session.next();
        session.failCurrent("LAN unavailable.");
        session.next();
        session.failCurrent("Tailscale unavailable.");

        assertEquals(0, session.successCount());
        assertTrue(session.failureSummary().contains("LAN unavailable"));
        assertTrue(session.failureSummary().contains("Tailscale unavailable"));
    }

    @Test
    public void successfulFallbackBecomesPreferredWhenPrimaryFails() {
        PairingRouteSession session = session(Collections.emptySet());
        session.next();
        session.failCurrent("LAN unavailable.");
        session.next();
        session.succeedCurrent();

        assertEquals("https://desktop.example.ts.net/agents-usage/", session.preferredBase());
    }

    private PairingRouteSession session(Set<String> healthy) {
        return new PairingRouteSession(
                List.of(EndpointParser.parse(LAN), EndpointParser.parse(TAILSCALE)), healthy);
    }
}
