package probe;

import java.io.File;
import java.lang.reflect.Method;
import java.net.URL;
import java.net.URLClassLoader;
import java.util.Arrays;

public class DynClassSem {
    static void p(String key, Object value) {
        System.out.println(key + " " + value);
    }

    public static void main(String[] args) throws Exception {
        DynBase base = new DynBase();
        int total = 0;
        for (int i = 0; i < 2048; i++) total += base.value();
        p("base", total);

        URLClassLoader loader = new URLClassLoader(
            new URL[] { new File(args[0]).toURI().toURL() },
            DynClassSem.class.getClassLoader());
        Class<?> pluginClass = Class.forName("dyn.Plugin", true, loader);
        Object plugin = pluginClass.getConstructor().newInstance();

        p("loaded", pluginClass.getName());
        p("static", pluginClass.getField("VALUE").get(null));
        p("virtual", ((DynBase) plugin).value());
        p("default", ((DynApi) plugin).label());

        Method add = pluginClass.getMethod("add", int.class);
        p("reflect", add.invoke(plugin, 5));
        p("modifiers", add.getModifiers());
        Method[] declared = pluginClass.getDeclaredMethods();
        String[] names = new String[declared.length];
        for (int i = 0; i < declared.length; i++) names[i] = declared[i].getName();
        Arrays.sort(names);
        p("methods", Arrays.toString(names));

        try {
            ((DynApi) plugin).fail();
        } catch (Throwable error) {
            p("throw", error.getClass().getName() + ":" + error.getMessage());
        }

        // DIVERGES: init.first — Rava currently propagates the original exception.
        try {
            Class.forName("dyn.BadInit", true, loader);
        } catch (Throwable error) {
            p("init.first", error.getClass().getName());
        }
        try {
            Class.forName("dyn.BadInit", true, loader);
        } catch (Throwable error) {
            p("init.retry", error.getClass().getName());
        }
        try {
            Class.forName("dyn.Missing", true, loader);
        } catch (Throwable error) {
            p("missing", error.getClass().getName());
        }
    }
}
