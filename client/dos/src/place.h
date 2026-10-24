/* SPDX-License-Identifier: MIT */
/*
 * Putting a received file in place without risking the file it replaces:
 * for the client's get. Portable (stdio and fsys.h), so the failure paths
 * are tested on the host.
 */
#ifndef SPROCKET_PLACE_H
#define SPROCKET_PLACE_H

/*
 * A path in the directory of dest (a native path) for name with its
 * extension replaced by one no file has yet: .$M$, or else .$M0 to .$M9,
 * where M is mark. Written to out (FS_PATH_MAX + 1 bytes). Returns 0 if
 * none is free or the path does not fit.
 */
int place_free_name(char *out, const char *dest, const char *name, char mark);

/*
 * Put the complete file temp in place as dest. An existing dest is first
 * renamed to a free backup name (mark B), then temp to dest, and only then
 * is the backup deleted. If renaming temp fails, the backup is renamed
 * back. Returns 1, or 0 with dest as it was and temp left for the caller.
 */
int place_file(const char *temp, const char *dest);

#endif
