package io.github.agentsusagetray.companion;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import java.net.URLEncoder;
import java.nio.charset.StandardCharsets;
import java.util.List;
import org.junit.Test;

public class EndpointParserTest {
    private static final String TOKEN = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    @Test
    public void parsesLanAddress() {
        EndpointParser.ParsedEndpoint endpoint = EndpointParser.parse("http://192.168.1.20:3765/pair?token=" + TOKEN);
        assertEquals("http://192.168.1.20:3765/", endpoint.baseUrl);
        assertEquals("192.168.1.20:3765", endpoint.displayName);
    }

    @Test
    public void preservesTailscaleServePrefix() {
        EndpointParser.ParsedEndpoint endpoint = EndpointParser.parse("https://desktop.example.ts.net/agents-usage/pair?token=" + TOKEN);
        assertEquals("https://desktop.example.ts.net/agents-usage/", endpoint.baseUrl);
        assertTrue(EndpointParser.belongsToBase("https://desktop.example.ts.net/agents-usage/api/state", endpoint.baseUrl));
        assertFalse(EndpointParser.belongsToBase("https://desktop.example.ts.net/other", endpoint.baseUrl));
    }

    @Test
    public void unwrapsAppLink() {
        String webUrl = "https://desktop.example.ts.net/agents-usage/pair?token=" + TOKEN;
        String appUrl = "agents-usage://pair?url=" + URLEncoder.encode(webUrl, StandardCharsets.UTF_8);
        assertEquals(webUrl, EndpointParser.parse(appUrl).pairingUrl);
    }

    @Test
    public void parsesLanAndTailscaleFromOneAppLink() {
        String lan = "http://192.168.1.20:3765/pair?token=" + TOKEN + "&path=/";
        String tail = "https://desktop.example.ts.net/agents-usage/pair?token=" + TOKEN
                + "&path=/agents-usage/";
        String appLink = "agents-usage://pair?url=" + URLEncoder.encode(lan, StandardCharsets.UTF_8)
                + "&fallback=" + URLEncoder.encode(tail, StandardCharsets.UTF_8);
        List<EndpointParser.ParsedEndpoint> endpoints = EndpointParser.parseAll(appLink);
        assertEquals(2, endpoints.size());
        assertEquals("http://192.168.1.20:3765/", endpoints.get(0).baseUrl);
        assertEquals("https://desktop.example.ts.net/agents-usage/", endpoints.get(1).baseUrl);
    }

    @Test
    public void parsesCompactDesktopBundle() {
        String appLink = "agents-usage://pair?token=" + TOKEN
                + "&base=" + URLEncoder.encode("http://192.168.1.20:3765/", StandardCharsets.UTF_8)
                + "&fallback=" + URLEncoder.encode(
                        "https://desktop.example.ts.net/agents-usage/", StandardCharsets.UTF_8);
        List<EndpointParser.ParsedEndpoint> endpoints = EndpointParser.parseAll(appLink);
        assertEquals(2, endpoints.size());
        assertEquals("http://192.168.1.20:3765/", endpoints.get(0).baseUrl);
        assertEquals("https://desktop.example.ts.net/agents-usage/", endpoints.get(1).baseUrl);
    }

    @Test
    public void allowsTailscaleAndPrivateRangesOverHttp() {
        EndpointParser.parse("http://100.100.20.30:3765/pair?token=" + TOKEN);
        EndpointParser.parse("http://10.0.0.2:3765/pair?token=" + TOKEN);
        EndpointParser.parse("http://[fd7a:115c:a1e0::1]:3765/pair?token=" + TOKEN);
    }

    @Test
    public void rejectsPublicCleartextAndMalformedLinks() {
        assertThrows(IllegalArgumentException.class,
                () -> EndpointParser.parse("http://example.com/pair?token=" + TOKEN));
        assertThrows(IllegalArgumentException.class,
                () -> EndpointParser.parse("http://8.8.8.8/pair?token=" + TOKEN));
        assertThrows(IllegalArgumentException.class,
                () -> EndpointParser.parse("https://example.com/not-pairing?token=" + TOKEN));
        assertThrows(IllegalArgumentException.class,
                () -> EndpointParser.parse("https://example.com/pair?token=short"));
    }
}
