//! `java.desktop` natives a headless VM needs: the `initIDs` of the AWT classes, which in the
//! JDK's libawt only cache JNI field and method IDs (the VM resolves members itself).

use super::{ret_void, NativeRegistry};

pub(super) fn register(r: &mut NativeRegistry) {
    for class in [
        "java/awt/Toolkit",
        "java/awt/Color",
        "java/awt/Rectangle",
        "java/awt/Dimension",
        "java/awt/Insets",
        "java/awt/Font",
        "java/awt/AWTEvent",
        "java/awt/Component",
        "java/awt/image/BufferedImage",
        "java/awt/image/ColorModel",
        "java/awt/image/IndexColorModel",
        "java/awt/image/Raster",
        "java/awt/image/SampleModel",
        "java/awt/image/SinglePixelPackedSampleModel",
        "java/awt/image/Kernel",
        "sun/awt/image/ByteComponentRaster",
        "sun/awt/image/BytePackedRaster",
        "sun/awt/image/ShortComponentRaster",
        "sun/awt/image/IntegerComponentRaster",
        "sun/awt/image/ImagingLib",
    ] {
        r.add(class, "initIDs", "()V", |_, _| ret_void());
    }
}
