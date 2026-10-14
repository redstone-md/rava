package ravabench;

import com.destroystokyo.paper.event.server.ServerTickEndEvent;
import java.io.BufferedWriter;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import java.util.Arrays;
import java.util.Locale;
import org.bukkit.ServerTickManager;
import org.bukkit.command.Command;
import org.bukkit.command.CommandSender;
import org.bukkit.command.ConsoleCommandSender;
import org.bukkit.event.EventHandler;
import org.bukkit.event.EventPriority;
import org.bukkit.event.Listener;
import org.bukkit.plugin.java.JavaPlugin;

/** External Paper observer; the VM does not recognize this plugin or its workload. */
public final class TickMeasure extends JavaPlugin implements Listener {
    private double[] durations;
    private int[] tickNumbers;
    private boolean[] sprinting;
    private ServerTickManager tickManager;
    private int warmup, warmupLeft, count, playersAtArm;
    private boolean running, skipCommandTick, writing;
    private String phase, mode;

    @Override public void onEnable() {
        getServer().getPluginManager().registerEvents(this, this);
        tickManager = getServer().getServerTickManager();
    }

    @Override public boolean onCommand(CommandSender sender, Command command, String label, String[] args) {
        if (!(sender instanceof ConsoleCommandSender)) {
            sender.sendMessage("This measurement command is console-only.");
            return true;
        }
        try {
            if ((args.length != 4 && args.length != 5) || !args[0].equals("start") || !args[1].matches("[A-Za-z0-9_-]{1,40}"))
                throw new IllegalArgumentException("tickmeasure start LABEL WARMUP_TICKS MEASURE_TICKS [paced|sprint]");
            if (running || writing) throw new IllegalArgumentException("A capture is still active.");
            int warm = Integer.parseInt(args[2]), n = Integer.parseInt(args[3]);
            if (warm < 0 || warm > 2_000_000 || n < 1 || n > 2_000_000)
                throw new IllegalArgumentException("Counts must be bounded by 2000000; measurement must be positive.");
            Path dir = getDataFolder().toPath();
            Files.createDirectories(dir);
            if (Files.exists(dir.resolve(args[1] + ".csv")))
                throw new IllegalArgumentException("Phase already exists.");
            phase = args[1];
            mode = args.length == 5 ? args[4] : "paced";
            if ((!mode.equals("paced") && !mode.equals("sprint")) || tickManager.isFrozen()
                    || tickManager.isStepping() || tickManager.isSprinting() || tickManager.getTickRate() != 20.0f)
                throw new IllegalArgumentException("Capture needs normal 20 TPS and paced|sprint mode.");
            durations = new double[n];
            tickNumbers = new int[n];
            sprinting = new boolean[n];
            warmup = warmupLeft = warm;
            count = 0;
            playersAtArm = getServer().getOnlinePlayers().size();
            if (mode.equals("sprint")) {
                // The return value describes the previous sprint, not whether this one started.
                tickManager.requestGameToSprint(warm + n + 2);
                if (!tickManager.isSprinting()) throw new IllegalArgumentException("Could not start requested sprint.");
            }
            skipCommandTick = true;
            running = true;
            getLogger().info("TICKMEASURE_ARMED " + phase);
        } catch (IllegalArgumentException | IOException e) {
            getLogger().severe("TICKMEASURE_ERROR " + e.getMessage());
        }
        return true;
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void endTick(ServerTickEndEvent event) {
        if (!running) return;
        if (skipCommandTick) { skipCommandTick = false; return; }
        if (warmupLeft > 0) { warmupLeft--; return; }
        durations[count] = event.getTickDuration();
        sprinting[count] = tickManager.isSprinting();
        tickNumbers[count++] = event.getTickNumber();
        if (count == durations.length) {
            running = false;
            writing = true;
            // All allocation, world access, sorting and I/O happen after the captured window.
            getServer().getScheduler().runTask(this, this::writeResults);
        }
    }

    private static double quantile(double[] sorted, double fraction) {
        return sorted[Math.max(0, (int) Math.ceil(fraction * sorted.length) - 1)];
    }

    private void writeResults() {
        try {
            if (mode.equals("sprint")) tickManager.stopSprinting();
            Path dir = getDataFolder().toPath();
            try (BufferedWriter out = Files.newBufferedWriter(dir.resolve(phase + ".csv"), StandardOpenOption.CREATE_NEW)) {
                out.write("tick,duration_ms,sprinting\n");
                for (int i = 0; i < count; i++) {
                    out.write(Integer.toString(tickNumbers[i])); out.write(',');
                    out.write(Double.toString(durations[i])); out.write(',');
                    out.write(Boolean.toString(sprinting[i])); out.newLine();
                }
            }
            double[] sorted = durations.clone();
            Arrays.sort(sorted);
            double sum = 0;
            int over50 = 0, gaps = 0, invalid = 0, sprintTicks = 0;
            for (int i = 0; i < count; i++) {
                double d = durations[i];
                if (!Double.isFinite(d) || d < 0) invalid++;
                sum += d;
                if (d > 50) over50++;
                if (sprinting[i]) sprintTicks++;
                if (i > 0 && tickNumbers[i] != tickNumbers[i - 1] + 1) gaps++;
            }
            int entities = getServer().getWorlds().stream().mapToInt(w -> w.getEntities().size()).sum();
            String json = String.format(Locale.ROOT,
                "{\"phase\":\"%s\",\"mode\":\"%s\",\"sprint_ticks\":%d,\"warmup_ticks\":%d,\"measure_ticks\":%d,\"first_tick\":%d,\"last_tick\":%d," +
                "\"mean_ms\":%.9f,\"p50_ms\":%.9f,\"p95_ms\":%.9f,\"p99_ms\":%.9f,\"max_ms\":%.9f," +
                "\"over_50ms\":%d,\"tick_gaps\":%d,\"invalid_durations\":%d,\"players_at_arm\":%d," +
                "\"players_after_capture\":%d,\"entities_after_capture\":%d}",
                phase, mode, sprintTicks, warmup, count, tickNumbers[0], tickNumbers[count - 1], sum / count,
                quantile(sorted, .50), quantile(sorted, .95), quantile(sorted, .99), sorted[count - 1],
                over50, gaps, invalid, playersAtArm, getServer().getOnlinePlayers().size(), entities);
            Files.writeString(dir.resolve(phase + ".json"), json + "\n", StandardOpenOption.CREATE_NEW);
            getLogger().info("TICKMEASURE_RESULT " + json);
        } catch (IOException e) {
            getLogger().severe("TICKMEASURE_ERROR " + e);
        } finally {
            writing = false;
        }
    }

    @Override public void onDisable() {
        if (running || writing) getLogger().severe("TICKMEASURE_ERROR incomplete capture at shutdown");
    }
}
