/* SPDX-License-Identifier: MIT */
/*
 * The server's file tree (docs/protocol-v1.1.md). Every request is
 * self-contained except writing, which keeps one file open between
 * FILE_BEGIN and FILE_COMMIT. Files arrive as NAME.$$$ and are put in
 * place only after the length and CRC-32 check, keeping the old version
 * as NAME.BAK (docs/protocol-v1.md section 5).
 */
#include <errno.h>
#include <stdio.h>
#include <string.h>

#include "crc32.h"
#include "fsys.h"
#include "proto.h"
#include "tree.h"

#define MAX_TREE_PATH 127
/* Bytes of an encoded entry besides its name. */
#define ENTRY_FIXED 11
#define FLAG_REPLACE 0x01
#define FLAG_MORE 0x01

static char root[FS_PATH_MAX + 1];
static uint8_t writable;

/* The path being worked on, its spelling in the tree, and its entry. */
static char native[FS_PATH_MAX + 1];
static char spelled[MAX_TREE_PATH + 1];
static struct fs_entry found;

/* The file being written. */
static FILE *up_file;
static char up_temp[FS_PATH_MAX + 1];
static char up_dest[FS_PATH_MAX + 1];
static char up_bak[FS_PATH_MAX + 1];
static uint8_t up_exists;
static uint32_t up_size;
static uint32_t up_next;
static uint32_t up_mtime;
static uint32_t up_crc;

static uint16_t get16(const uint8_t *p)
{
    return (uint16_t)(p[0] | (p[1] << 8));
}

static uint32_t get32(const uint8_t *p)
{
    return (uint32_t)get16(p) | ((uint32_t)get16(p + 2) << 16);
}

static void put32(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)(v & 0xFF);
    p[1] = (uint8_t)((v >> 8) & 0xFF);
    p[2] = (uint8_t)((v >> 16) & 0xFF);
    p[3] = (uint8_t)(v >> 24);
}

static int lower(int ch)
{
    return ch >= 'A' && ch <= 'Z' ? ch + ('a' - 'A') : ch;
}

/* Equal ignoring ASCII case. */
static int same_name(const char *a, const char *b, size_t blen)
{
    size_t i;

    for (i = 0; i < blen; i++) {
        if (a[i] == '\0' || lower(a[i]) != lower(b[i])) {
            return 0;
        }
    }
    return a[blen] == '\0';
}

void tree_init(const char *dir, int allow_writes)
{
    size_t len = strlen(dir);

    if (len > FS_PATH_MAX) {
        len = FS_PATH_MAX;
    }
    memcpy(root, dir, len);
    /* Without a trailing separator, unless that is all there is. */
    while (len > 1 && (root[len - 1] == '/' || root[len - 1] == '\\')
           && root[len - 2] != ':') {
        len--;
    }
    root[len] = '\0';
    writable = (uint8_t)(allow_writes != 0);
    up_file = NULL;
}

void tree_reset(void)
{
    if (up_file != NULL) {
        fclose(up_file);
        up_file = NULL;
        remove(up_temp);
    }
}

/* Append a separator and name to native; 0 if it does not fit. */
static int append(char *path, const char *name, size_t len)
{
    size_t have = strlen(path);

    if (have + 1 + len > FS_PATH_MAX) {
        return 0;
    }
    if (have > 0 && path[have - 1] != '/' && path[have - 1] != '\\') {
        path[have++] = FS_SEP;
    }
    memcpy(path + have, name, len);
    path[have + len] = '\0';
    return 1;
}

/*
 * Find the entry named name[0 .. len) in the directory at native: the
 * exact name if there is one, otherwise the one that differs only in case.
 * Sets found. Returns 0 or an ERROR class.
 */
static uint8_t find_entry(const char *name, size_t len)
{
    struct fs_entry e;
    uint16_t index;
    uint8_t similar = 0;
    int rc;

    for (index = 0;; index++) {
        rc = fs_scan(native, index, &e);
        if (rc == FS_END) {
            break;
        }
        if (rc != FS_OK) {
            return ERR_NOT_FOUND;
        }
        if (strlen(e.name) == len && memcmp(e.name, name, len) == 0) {
            found = e;
            return 0;
        }
        if (same_name(e.name, name, len)) {
            if (similar++ == 0) {
                found = e;
            }
        }
    }
    if (similar == 1) {
        return 0;
    }
    return similar ? ERR_AMBIGUOUS : ERR_NOT_FOUND;
}

/* A component a tree path may not hold (protocol v1.1 section 3). */
static int bad_component(const char *name, size_t len)
{
    size_t i;

    if (len == 0 || name[0] == '.') {
        return 1;
    }
    for (i = 0; i < len; i++) {
        if (name[i] == '\\' || name[i] == ':' || name[i] < 0x20 || name[i] > 0x7E) {
            return 1;
        }
    }
    return 0;
}

/*
 * Resolve a tree path to native, spelled and found. The path's last
 * component is left unresolved if leave_last, and is then returned in
 * *last (with its length): for FILE_BEGIN, where it may not exist yet.
 * Returns 0 or an ERROR class.
 */
static uint8_t resolve(const uint8_t *path, uint8_t len, int leave_last, const char **last,
                       size_t *last_len)
{
    const char *p = (const char *)path;
    const char *end = p + len;
    const char *slash;
    size_t n;
    uint8_t rc;

    if (len > MAX_TREE_PATH) {
        return ERR_NOT_FOUND;
    }
    strcpy(native, root);
    spelled[0] = '\0';
    found.dir = 1;
    found.size = 0;
    found.mtime = 0;
    if (p < end && *p == '/') {
        p++;
    }
    while (p < end) {
        slash = memchr(p, '/', (size_t)(end - p));
        n = slash ? (size_t)(slash - p) : (size_t)(end - p);
        if (bad_component(p, n) || (slash && slash + 1 == end)) {
            return ERR_NOT_FOUND;
        }
        if (!found.dir) {
            return ERR_NOT_A_DIRECTORY;
        }
        if (leave_last && slash == NULL) {
            *last = p;
            *last_len = n;
            return 0;
        }
        rc = find_entry(p, n);
        if (rc != 0) {
            return rc;
        }
        if (!append(native, found.name, strlen(found.name))
            || strlen(spelled) + 1 + strlen(found.name) > MAX_TREE_PATH) {
            return ERR_NOT_FOUND;
        }
        if (spelled[0] != '\0') {
            strcat(spelled, "/");
        }
        strcat(spelled, found.name);
        p += n + (slash ? 1 : 0);
    }
    if (leave_last) {
        /* The root itself cannot be written. */
        return ERR_IS_A_DIRECTORY;
    }
    return 0;
}

/* Encode an entry; returns its length. */
static uint16_t put_entry(uint8_t *out, const struct fs_entry *e, const char *name)
{
    size_t len = strlen(name);

    out[0] = (uint8_t)(10 + len);
    out[1] = e->dir;
    put32(out + 2, e->dir ? 0 : e->size);
    put32(out + 6, e->mtime);
    out[10] = (uint8_t)len;
    memcpy(out + 11, name, len);
    return (uint16_t)(ENTRY_FIXED + len);
}

/* A request's path_len and path at in[at]; 0 if it does not fit. */
static int path_field(const uint8_t *in, uint16_t in_len, uint16_t at, const uint8_t **path,
                      uint8_t *len)
{
    if (at >= in_len || (uint16_t)(at + 1 + in[at]) > in_len) {
        return 0;
    }
    *len = in[at];
    *path = in + at + 1;
    return 1;
}

static int fail(uint8_t *class, uint8_t value)
{
    *class = value;
    return -1;
}

static int stat_request(const uint8_t *in, uint16_t in_len, uint8_t *out, uint16_t limit,
                        uint8_t *class)
{
    const uint8_t *path;
    uint8_t len;
    uint8_t rc;

    if (!path_field(in, in_len, 0, &path, &len)) {
        return fail(class, ERR_INVALID_REQUEST);
    }
    rc = resolve(path, len, 0, NULL, NULL);
    if (rc != 0) {
        return fail(class, rc);
    }
    if (ENTRY_FIXED + strlen(spelled) > limit) {
        return fail(class, ERR_NOT_FOUND);
    }
    return put_entry(out, &found, spelled);
}

static int list_request(const uint8_t *in, uint16_t in_len, uint8_t *out, uint16_t limit,
                        uint8_t *class)
{
    const uint8_t *path;
    uint8_t len;
    uint8_t rc;
    uint16_t index;
    uint16_t used = 2;
    uint8_t count = 0;
    struct fs_entry e;
    int scan;

    if (in_len < 2 || !path_field(in, in_len, 2, &path, &len)) {
        return fail(class, ERR_INVALID_REQUEST);
    }
    index = get16(in);
    rc = resolve(path, len, 0, NULL, NULL);
    if (rc != 0) {
        return fail(class, rc);
    }
    if (!found.dir) {
        return fail(class, ERR_NOT_A_DIRECTORY);
    }
    out[0] = 0;
    for (;; index++) {
        scan = fs_scan(native, index, &e);
        if (scan == FS_END) {
            break;
        }
        if (scan != FS_OK) {
            return fail(class, ERR_NOT_FOUND);
        }
        if (used + ENTRY_FIXED + strlen(e.name) > limit || count == 255) {
            out[0] = FLAG_MORE;
            break;
        }
        used = (uint16_t)(used + put_entry(out + used, &e, e.name));
        count++;
    }
    out[1] = count;
    return used;
}

static int read_request(const uint8_t *in, uint16_t in_len, uint8_t *out, uint16_t limit,
                        uint8_t *class)
{
    const uint8_t *path;
    uint8_t len;
    uint8_t rc;
    uint32_t offset;
    uint16_t length;
    size_t got = 0;
    FILE *f;

    if (in_len < 6 || !path_field(in, in_len, 6, &path, &len)) {
        return fail(class, ERR_INVALID_REQUEST);
    }
    offset = get32(in);
    length = get16(in + 4);
    if (length > limit - 8) {
        length = (uint16_t)(limit - 8);
    }
    rc = resolve(path, len, 0, NULL, NULL);
    if (rc != 0) {
        return fail(class, rc);
    }
    if (found.dir) {
        return fail(class, ERR_IS_A_DIRECTORY);
    }
    put32(out, found.size);
    put32(out + 4, found.mtime);
    if (offset < found.size) {
        f = fopen(native, "rb");
        if (f == NULL) {
            return fail(class, ERR_FILE_ERROR);
        }
        if (fseek(f, (long)offset, SEEK_SET) == 0) {
            got = fread(out + 8, 1, length, f);
        }
        fclose(f);
    }
    return (int)(8 + got);
}

/* Name with its extension replaced by ext, in the directory of native. */
static int sibling(char *buf, const char *name, const char *ext)
{
    const char *dot = strrchr(name, '.');
    size_t base = dot ? (size_t)(dot - name) : strlen(name);
    char alt[FS_NAME_MAX + 5];

    if (base + strlen(ext) >= sizeof alt) {
        return 0;
    }
    memcpy(alt, name, base);
    strcpy(alt + base, ext);
    strcpy(buf, native);
    return append(buf, alt, strlen(alt));
}

static int begin_request(const uint8_t *in, uint16_t in_len, uint8_t *out, uint8_t *class,
                         uint16_t *code)
{
    const uint8_t *path;
    uint8_t len;
    uint8_t flags;
    uint8_t rc;
    const char *last = NULL;
    size_t last_len = 0;
    char name[FS_NAME_MAX + 1];
    struct fs_entry e;
    uint8_t found_bits = 0;

    if (!writable) {
        return fail(class, ERR_READ_ONLY);
    }
    tree_reset();
    if (in_len < 5 || !path_field(in, in_len, 5, &path, &len)) {
        return fail(class, ERR_INVALID_REQUEST);
    }
    up_size = get32(in);
    flags = in[4];
    if (flags & ~FLAG_REPLACE) {
        return fail(class, ERR_INVALID_REQUEST);
    }
    /* A trailing field from minor version 1. */
    up_mtime = (uint16_t)(6 + len + 4) <= in_len ? get32(in + 6 + len) : 0;
    rc = resolve(path, len, 1, &last, &last_len);
    if (rc != 0) {
        return fail(class, rc);
    }
    if (!found.dir) {
        return fail(class, ERR_NOT_A_DIRECTORY);
    }
    rc = find_entry(last, last_len);
    if (rc == ERR_AMBIGUOUS) {
        return fail(class, rc);
    }
    if (rc == 0) {
        if (found.dir) {
            return fail(class, ERR_IS_A_DIRECTORY);
        }
        if (!(flags & FLAG_REPLACE)) {
            return fail(class, ERR_EXISTS);
        }
        strcpy(name, found.name);
        up_exists = 1;
        found_bits |= 1;
    } else {
        if (last_len > FS_NAME_MAX) {
            return fail(class, ERR_BAD_NAME);
        }
        memcpy(name, last, last_len);
        name[last_len] = '\0';
        if (!fs_name_ok(name)) {
            return fail(class, ERR_BAD_NAME);
        }
        up_exists = 0;
    }
    strcpy(up_dest, native);
    if (!append(up_dest, name, strlen(name)) || !sibling(up_temp, name, ".$$$")
        || !sibling(up_bak, name, ".BAK")) {
        return fail(class, ERR_BAD_NAME);
    }
    if (fs_stat(up_bak, &e) == FS_OK) {
        found_bits |= 2;
    }
    if (fs_stat(up_temp, &e) == FS_OK) {
        remove(up_temp);
        found_bits |= 4;
    }
    up_file = fopen(up_temp, "wb");
    if (up_file == NULL) {
        *code = fs_error_code();
        return fail(class, ERR_FILE_ERROR);
    }
    up_next = 0;
    up_crc = CRC32_INIT;
    out[0] = found_bits;
    return 1;
}

static int data_request(const uint8_t *in, uint16_t in_len, uint8_t *class, uint16_t *code)
{
    uint16_t n = (uint16_t)(in_len - 4);

    if (up_file == NULL || in_len < 4 || get32(in) != up_next
        || (uint32_t)n > up_size - up_next) {
        return fail(class, ERR_INVALID_REQUEST);
    }
    if (fwrite(in + 4, 1, n, up_file) != n) {
        *code = fs_error_code();
        tree_reset();
        return fail(class, ERR_FILE_ERROR);
    }
    up_crc = crc32_update(up_crc, in + 4, n);
    up_next += n;
    return 0;
}

static int commit_request(const uint8_t *in, uint16_t in_len, uint8_t *out, uint8_t *class,
                          uint16_t *code)
{
    uint32_t crc = ~up_crc;
    int closed;

    if (up_file == NULL || in_len < 8) {
        return fail(class, ERR_INVALID_REQUEST);
    }
    /* Closing flushes DOS's buffers to the disk. */
    closed = fclose(up_file);
    up_file = NULL;
    if (closed != 0 || get32(in) != up_size || up_next != up_size || get32(in + 4) != crc) {
        remove(up_temp);
        return fail(class, closed != 0 ? ERR_FILE_ERROR : ERR_VERIFY_FAILED);
    }
    if (up_exists) {
        remove(up_bak);
        if (rename(up_dest, up_bak) != 0) {
            *code = fs_error_code();
            remove(up_temp);
            return fail(class, ERR_FILE_ERROR);
        }
    }
    if (rename(up_temp, up_dest) != 0) {
        *code = fs_error_code();
        if (up_exists) {
            rename(up_bak, up_dest);
        }
        remove(up_temp);
        return fail(class, ERR_FILE_ERROR);
    }
    if (up_mtime != 0) {
        fs_set_mtime(up_dest, up_mtime);
    }
    put32(out, crc);
    return 4;
}

int tree_request(uint8_t kind, const uint8_t *in, uint16_t in_len, uint8_t *out,
                 uint16_t limit, uint8_t *class, uint16_t *code)
{
    *code = 0;
    switch (kind) {
    case MSG_STAT:
        return stat_request(in, in_len, out, limit, class);
    case MSG_DIR_LIST:
        return list_request(in, in_len, out, limit, class);
    case MSG_FILE_READ:
        return read_request(in, in_len, out, limit, class);
    case MSG_FILE_BEGIN:
        return begin_request(in, in_len, out, class, code);
    case MSG_FILE_DATA:
        return writable ? data_request(in, in_len, class, code) : fail(class, ERR_READ_ONLY);
    case MSG_FILE_COMMIT:
        return writable ? commit_request(in, in_len, out, class, code)
                        : fail(class, ERR_READ_ONLY);
    case MSG_FILE_ABORT:
        if (!writable) {
            return fail(class, ERR_READ_ONLY);
        }
        tree_reset();
        return 0;
    default:
        return fail(class, ERR_UNSUPPORTED);
    }
}
