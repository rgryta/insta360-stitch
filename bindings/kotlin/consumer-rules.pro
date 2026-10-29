# R8 rules for apps that use the library. The Rust bridge finds these by name through JNI: R8 must not remove or
# rename them.
# The exception that the bridge throws for an unsupported file, with its (String) constructor.
-keep class eu.gryta.insta360stitch.Insta360UnsupportedException { <init>(java.lang.String); }
# The native methods: their JNI names come from the class name and the method names.
-keepclasseswithmembernames class eu.gryta.insta360stitch.Native { native <methods>; }
