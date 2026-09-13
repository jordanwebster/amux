#include "include/LaunchClock.h"

#include <mach/mach_time.h>

static uint64_t loaded_at = 0;

__attribute__((constructor))
static void amux_note_images_loaded(void) {
    loaded_at = mach_absolute_time();
}

uint64_t amux_images_loaded_at(void) {
    return loaded_at;
}
