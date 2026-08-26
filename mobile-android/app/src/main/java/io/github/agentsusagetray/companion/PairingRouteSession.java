package io.github.agentsusagetray.companion;

import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;

final class PairingRouteSession {
    private final List<EndpointParser.ParsedEndpoint> pending = new ArrayList<>();
    private final Set<String> successfulBases = new LinkedHashSet<>();
    private final List<String> failures = new ArrayList<>();
    private final String preferredBase;
    private EndpointParser.ParsedEndpoint current;
    private String lastSuccessfulBase;

    PairingRouteSession(
            List<EndpointParser.ParsedEndpoint> routes,
            Set<String> alreadyHealthyBases) {
        if (routes == null || routes.isEmpty()) {
            throw new IllegalArgumentException("At least one pairing route is required.");
        }
        preferredBase = routes.get(0).baseUrl;
        for (EndpointParser.ParsedEndpoint route : routes) {
            if (alreadyHealthyBases.contains(route.baseUrl)) {
                successfulBases.add(route.baseUrl);
                lastSuccessfulBase = route.baseUrl;
            } else {
                pending.add(route);
            }
        }
    }

    EndpointParser.ParsedEndpoint next() {
        if (current != null) throw new IllegalStateException("The current route is not finished.");
        if (pending.isEmpty()) return null;
        current = pending.remove(0);
        return current;
    }

    void succeedCurrent() {
        if (current == null) throw new IllegalStateException("There is no current route.");
        successfulBases.add(current.baseUrl);
        lastSuccessfulBase = current.baseUrl;
        current = null;
    }

    void failCurrent(String reason) {
        if (current == null) throw new IllegalStateException("There is no current route.");
        failures.add(current.displayName + ": " + reason);
        current = null;
    }

    int successCount() {
        return successfulBases.size();
    }

    boolean hasFailures() {
        return !failures.isEmpty();
    }

    String failureSummary() {
        return String.join("\n", failures);
    }

    String preferredBase() {
        return successfulBases.contains(preferredBase) ? preferredBase : lastSuccessfulBase;
    }
}
