#ifndef AMUX_LAUNCH_CLOCK_H
#define AMUX_LAUNCH_CLOCK_H

#include <stdint.h>

/// `mach_absolute_time()` at the moment the dynamic linker had finished
/// mapping, binding and initialising every image this app is built out of.
///
/// Swift has no way to run code that early: the earliest a Swift declaration
/// can observe is the first time something touches it, which on a launch is
/// already after UIKit has started. A C image initialiser runs at the end of
/// the linker's work and before `main()`, which is exactly the line that
/// separates loading the app from running it, and telling those two apart is
/// what lets a launch that got slower be blamed on the right half.
///
/// Zero if the initialiser never ran, which should not happen and is reported
/// as an unknown rather than as a zero-length load.
///
/// Read through a function rather than as a variable: it is written once,
/// before any thread exists, but Swift can only be told that about a call.
uint64_t amux_images_loaded_at(void);

#endif
