# Minecraft kernels

Microbenchmarks that run the Paper server jar's own classes. They compare the steady-state code
quality of `rava` and `java` on world-generation kernels in seconds, without a full server start.

```
B=<paper base>    # e.g. the directory bench/paper_ab.sh uses (versions/, libraries/ unpacked)
CP=$B/versions/1.21.1/paper-1.21.1.jar:$(find $B/libraries -name '*.jar' | tr '\n' :)
javac -cp "$CP" -d out bench/mc/*.java
java -cp "out:$CP" SplineBench    # or McNoiseBench,
rava -cp "out:$CP" SplineBench    #    net.minecraft.world.level.levelgen.ShiftedNoiseBench
```

Each one prints ns per operation after warm-up, and a checksum that must match between VMs.
