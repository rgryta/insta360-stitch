/*
 * The C API of the insta360-stitch bridge, for Kotlin/Native (iOS). src/capi.rs implements it.
 *
 * Each function that can fail returns a status (I360_OK or an error code) and writes the error message to *error.
 * Free the message with i360_string_free. A null pointer or a string that is not UTF-8 is I360_INVALID_ARGUMENT.
 */
#ifndef INSTA360_STITCH_H
#define INSTA360_STITCH_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define I360_OK 0
/* The library cannot stitch the file. A retry will not help. */
#define I360_UNSUPPORTED 1
/* A read operation failed. A retry can help. */
#define I360_IO 2
/* The calling code gave a wrong value. */
#define I360_INVALID_ARGUMENT 3
/* An internal error. */
#define I360_PANIC 4

typedef struct I360File I360File;

/* Bytes that the library allocated. Free them with i360_buffer_free. */
typedef struct I360Buffer {
    uint8_t *data;
    size_t len;
    uint32_t width;
    uint32_t height;
} I360Buffer;

/* The remap planes: x0, y0, x1 and y1 are little-endian uint16 planes, mask is the blend weight of lens 0. */
typedef struct I360VideoMaps {
    uint32_t width;
    uint32_t height;
    I360Buffer x0;
    I360Buffer y0;
    I360Buffer x1;
    I360Buffer y1;
    I360Buffer mask;
} I360VideoMaps;

int32_t i360_open(const char *path, I360File **out, char **error);
/* Free a file one time, after all other calls on it are complete. */
void i360_free(I360File *file);

/* These give a string to free with i360_string_free, or NULL. */
char *i360_unsupported_reason(const I360File *file);
char *i360_local_time(const I360File *file);
int32_t i360_offset_state(const I360File *file);
bool i360_preview_size(const I360File *file, uint32_t *width, uint32_t *height);

int32_t i360_preview(const I360File *file, I360Buffer *out, char **error);
/* Stitches the rows first_row to first_row + rows - 1 into out (rows * width * channels bytes). */
int32_t i360_stitch_into(const I360File *file, const uint8_t *raw, size_t raw_len, int32_t width, int32_t height,
                         int32_t channels, int32_t first_row, int32_t rows, uint8_t *out, size_t out_len,
                         char **error);
/* width 0 is the default width (2 * lens_width). */
int32_t i360_video_maps(const I360File *file, int32_t lens_width, int32_t lens_height, int32_t width,
                        I360VideoMaps *out, char **error);
int32_t i360_add_spherical_metadata(const char *input, const char *output, char **error);
bool i360_camera_utc_offset_minutes(const char *local_time, int64_t utc_unix_seconds, int32_t *minutes);
/* Do not free this string. */
const char *i360_ffmpeg_filter(void);

void i360_string_free(char *value);
void i360_buffer_free(I360Buffer buffer);
void i360_video_maps_free(I360VideoMaps maps);

#endif
