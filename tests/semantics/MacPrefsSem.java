// The macOS CFPreferences backend: Unicode, empty keys, nested trees and node removal.
// Array ordering is unspecified by Preferences, so enumeration output is sorted.
import java.util.Arrays;
import java.util.logging.Level;
import java.util.logging.Logger;
import java.util.prefs.Preferences;

public class MacPrefsSem {
    static String sorted(String[] names) { Arrays.sort(names); return Arrays.toString(names); }

    public static void main(String[] args) throws Exception {
        if (!System.getProperty("os.name").startsWith("Mac")) {
            System.out.println("mac prefs skipped");
            return;
        }
        Logger.getLogger("java.util.prefs").setLevel(Level.OFF);
        Preferences root = Preferences.userRoot();
        String name = "rava-macos-probe-" + ProcessHandle.current().pid();
        Preferences top = root.node(name);
        try {
            top.put("", "empty-value");
            System.out.println("empty " + top.get("", "missing"));
            top.remove("");
            System.out.println("empty removed " + top.get("", "missing"));
            top.put("žluťoučký", "🐱\u0000東京");
            top.put("alpha", "first");
            top.putInt("count", 42);
            System.out.println("keys " + sorted(top.keys()));
            System.out.println("unicode " + top.get("žluťoučký", "missing").equals("🐱\u0000東京"));

            Preferences branch = top.node("branch");
            Preferences leaf = branch.node("leaf");
            leaf.put("deep", "value");
            top.node("other").putBoolean("enabled", true);
            root.flush();
            top.sync();
            System.out.println("children " + sorted(top.childrenNames()));
            System.out.println("nested " + sorted(branch.childrenNames()) + " " + leaf.get("deep", "missing"));
            System.out.println("values " + top.getInt("count", -1) + " " + top.node("other").getBoolean("enabled", false));
            leaf.remove("deep");
            leaf.sync();
            System.out.println("key removed " + leaf.get("deep", "missing"));
            // This node's depth crosses the JDK's three-component CF domain boundary.
            leaf.removeNode();
            root.flush();
            System.out.println("leaf removed " + branch.nodeExists("leaf"));
        } finally {
            top.removeNode();
            root.flush();
        }
        System.out.println("top removed " + root.nodeExists(name));
    }
}
