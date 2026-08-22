package io.github.agentsusagetray.companion;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNotNull;

import androidx.test.core.app.ActivityScenario;
import androidx.test.ext.junit.runners.AndroidJUnit4;
import org.junit.Test;
import org.junit.runner.RunWith;

@RunWith(AndroidJUnit4.class)
public final class MainActivityTest {
    @Test
    public void testSetupActivityStartsWithoutAnAccountOrProvider() {
        try (ActivityScenario<MainActivity> scenario = ActivityScenario.launch(MainActivity.class)) {
            scenario.onActivity(activity -> {
                assertNotNull(activity);
                assertEquals("io.github.agentsusagetray.companion.debug", activity.getPackageName());
            });
        }
    }
}
