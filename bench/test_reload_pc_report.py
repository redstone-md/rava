import unittest

from reload_pc_report import ReloadProfile


class ReloadProfileTest(unittest.TestCase):
    def setUp(self):
        names = ["registries.start", "cold.start", "cold.end", "warm.start",
                 "warm.end", "warm.digest.end", "cold.digest.end"]
        self.output = "\n".join(f"RELOAD_PHASE name={name} epoch_ms={1000 + i * 10}"
                                for i, name in enumerate(names))
        self.output += ("\nWARM_RESULT index=0 HASH=c956377e19071d7a\n"
                        "RELOAD_MS=10 HASH=c956377e19071d7a\n")
        self.trace = "\n".join(f"[PC-TRACE] {stamp} {leaf} |" for stamp, leaf in [
            (1005, "registry"), (1015, "cold"), (1025, "between"),
            (1035, "warm"), (1040, "digest_boundary"), (1045, "warm_digest"),
            (1055, "cold_digest")])

    def test_digest_samples_never_enter_reload_window(self):
        reports = {r["phase"]: r for r in ReloadProfile(self.output, self.trace).report()}
        self.assertEqual(reports["warm.0"]["samples"], 1)
        self.assertEqual(reports["warm.0"]["symbols"][0]["name"], "warm")
        self.assertEqual(reports["warm.digest.0"]["samples"], 2)
        self.assertEqual(reports["cold"]["samples"], 1)

    def test_incomplete_or_incorrect_certification_is_rejected(self):
        for output in [self.output.replace("WARM_RESULT", "MISSING"),
                       self.output.replace("c956377e19071d7a", "0000000000000000"),
                       self.output.replace("index=0", "index=1"),
                       self.output.replace("warm.end", "missing.end"),
                       self.output.replace("epoch_ms=1040", "epoch_ms=999")]:
            with self.subTest(output=output), self.assertRaises(ValueError):
                ReloadProfile(output, self.trace).report()

    def test_absent_samples_in_a_window_are_not_a_zero_cost_claim(self):
        with self.assertRaises(ValueError):
            ReloadProfile(self.output, self.trace.replace("[PC-TRACE] 1035", "[IGNORED] 1035")).report()


if __name__ == "__main__":
    unittest.main()
