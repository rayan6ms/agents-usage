package io.github.agentsusagetray.companion;

import static org.junit.Assert.assertEquals;

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.atomic.AtomicReference;
import org.junit.Test;

public final class EndpointHealthProbeTest {
    @Test
    public void sendsTheSessionCookieAndAcceptsOnlyTheRealHealthResponse() throws Exception {
        AtomicReference<String> cookie = new AtomicReference<>();
        ServerSocket server = new ServerSocket();
        server.bind(new InetSocketAddress("127.0.0.1", 0));
        Thread responder = new Thread(() -> {
            try (Socket socket = server.accept();
                    BufferedReader reader = new BufferedReader(new InputStreamReader(
                            socket.getInputStream(), StandardCharsets.US_ASCII));
                    OutputStream output = socket.getOutputStream()) {
                for (String line = reader.readLine(); line != null && !line.isEmpty(); line = reader.readLine()) {
                    if (line.regionMatches(true, 0, "Cookie:", 0, "Cookie:".length())) {
                        cookie.set(line.substring("Cookie:".length()).trim());
                    }
                }
                output.write("HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"
                        .getBytes(StandardCharsets.US_ASCII));
                output.flush();
            } catch (Exception error) {
                throw new AssertionError(error);
            }
        });
        responder.start();
        try {
            String base = "http://127.0.0.1:" + server.getLocalPort() + "/";
            assertEquals(204, EndpointHealthProbe.check(base, "agents_usage_mobile=session", 1000));
            responder.join(1000);
            assertEquals("agents_usage_mobile=session", cookie.get());
        } finally {
            server.close();
        }
    }

    @Test
    public void reportsAnUnreachableEndpointWithoutThrowing() {
        assertEquals(-1, EndpointHealthProbe.check("http://127.0.0.1:1/", null, 100));
    }
}
