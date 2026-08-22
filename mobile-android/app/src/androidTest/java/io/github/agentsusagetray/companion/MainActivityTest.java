package io.github.agentsusagetray.companion;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNotNull;

import android.content.Intent;
import android.content.pm.ResolveInfo;

import androidx.test.core.app.ActivityScenario;
import androidx.test.ext.junit.runners.AndroidJUnit4;
import java.util.List;
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
                assertEquals("Agents Usage", activity.getApplicationInfo().loadLabel(activity.getPackageManager()));
                Intent launcher = new Intent(Intent.ACTION_MAIN)
                        .addCategory(Intent.CATEGORY_LAUNCHER)
                        .setPackage(activity.getPackageName());
                List<ResolveInfo> launcherActivities = activity.getPackageManager().queryIntentActivities(launcher, 0);
                assertEquals(1, launcherActivities.size());
                assertEquals(MainActivity.class.getName(), launcherActivities.get(0).activityInfo.name);
            });
        }
    }
}
