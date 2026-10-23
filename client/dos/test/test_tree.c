/* SPDX-License-Identifier: MIT */
/*
 * Runs the file tree (src/tree.c, on src/posixfs.c) against the v1.1
 * fixtures in tests/vectors/frames.txt and a scratch directory: lookups,
 * paging, refusals, and writing with its temporary and backup files.
 *
 * Usage: test_tree VECTOR_DIR
 */
#define _DEFAULT_SOURCE
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "../src/crc32.h"
#include "../src/frame.h"
#include "../src/fsys.h"
#include "../src/proto.h"
#include "../src/tree.h"
#include "vectors.h"

#define LIMIT 1024
/* DOS time of the fixtures: 2026-10-23 14:30:58. */
#define MTIME 0x5D5773DDUL

static char frames_path[512];
static char dir[256];
static uint8_t resp_buf[LIMIT + FRAME_HEADER_LEN];
static struct client cl;

/* The last tree_request. */
static uint8_t out[LIMIT];
static uint8_t class;

static void put32(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
    p[2] = (uint8_t)(v >> 16);
    p[3] = (uint8_t)(v >> 24);
}

static uint32_t get32(const uint8_t *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16)
         | ((uint32_t)p[3] << 24);
}

static char *in_dir(const char *name)
{
    static char path[512];

    snprintf(path, sizeof path, "%s/%s", dir, name);
    return path;
}

static void write_file(const char *name, const void *data, size_t len, uint32_t mtime)
{
    FILE *f = fopen(in_dir(name), "wb");

    if (f == NULL || fwrite(data, 1, len, f) != len || fclose(f) != 0) {
        printf("cannot write %s\n", in_dir(name));
        exit(1);
    }
    if (mtime != 0) {
        fs_set_mtime(in_dir(name), mtime);
    }
}

static int exists(const char *name)
{
    return access(in_dir(name), F_OK) == 0;
}

static void make_tree(void)
{
    static uint8_t doom[1020];

    strcpy(dir, "/tmp/test_tree.XXXXXX");
    if (mkdtemp(dir) == NULL) {
        printf("cannot make a scratch directory\n");
        exit(1);
    }
    mkdir(in_dir("docs"), 0777);
    mkdir(in_dir("GAMES"), 0777);
    mkdir(in_dir("case"), 0777);
    write_file("docs/readme.txt", "hello", 5, MTIME);
    memcpy(doom + 1016, "\x7e\x7d\x00\xff", 4);
    write_file("GAMES/DOOM.EXE", doom, sizeof doom, MTIME);
    write_file("A.TXT", "abc", 3, MTIME);
    write_file("case/x.txt", "1", 1, 0);
    write_file("case/X.TXT", "2", 1, 0);
}

static void remove_tree(void)
{
    char command[300];

    snprintf(command, sizeof command, "rm -rf '%s'", dir);
    if (system(command) != 0) {
        printf("cannot remove %s\n", dir);
    }
}

/* A request with a path at the end of payload[0 .. fixed). */
static int request(uint8_t kind, const uint8_t *fixed, uint16_t fixed_len, const char *path,
                   uint16_t limit)
{
    uint8_t in[300];
    uint16_t code;
    size_t len = strlen(path);

    memcpy(in, fixed, fixed_len);
    in[fixed_len] = (uint8_t)len;
    memcpy(in + fixed_len + 1, path, len);
    class = 0;
    return tree_request(kind, in, (uint16_t)(fixed_len + 1 + len), out, limit, &class, &code);
}

static int stat_path(const char *path)
{
    return request(MSG_STAT, NULL, 0, path, LIMIT);
}

static int begin(const char *path, uint32_t size, uint8_t flags)
{
    uint8_t fixed[5];
    uint8_t in[300];
    uint16_t code;
    size_t len = strlen(path);

    put32(fixed, size);
    fixed[4] = flags;
    memcpy(in, fixed, 5);
    in[5] = (uint8_t)len;
    memcpy(in + 6, path, len);
    put32(in + 6 + len, MTIME);
    class = 0;
    return tree_request(MSG_FILE_BEGIN, in, (uint16_t)(10 + len), out, LIMIT, &class, &code);
}

static int data(uint32_t offset, const char *text)
{
    uint8_t in[64];
    uint16_t code;
    size_t len = strlen(text);

    put32(in, offset);
    memcpy(in + 4, text, len);
    class = 0;
    return tree_request(MSG_FILE_DATA, in, (uint16_t)(4 + len), out, LIMIT, &class, &code);
}

static int commit(uint32_t size, uint32_t crc)
{
    uint8_t in[8];
    uint16_t code;

    put32(in, size);
    put32(in + 4, crc);
    class = 0;
    return tree_request(MSG_FILE_COMMIT, in, 8, out, LIMIT, &class, &code);
}

static uint32_t crc_of(const char *text)
{
    return ~crc32_update(CRC32_INIT, (const uint8_t *)text, (uint16_t)strlen(text));
}

static int file_is(const char *name, const char *text)
{
    char buf[64];
    FILE *f = fopen(in_dir(name), "rb");
    size_t len;

    if (f == NULL) {
        return 0;
    }
    len = fread(buf, 1, sizeof buf, f);
    fclose(f);
    return len == strlen(text) && memcmp(buf, text, len) == 0;
}

/* The response to fixture `request` must be fixture `response`. */
static void exchange(const char *request_name, const char *response_name)
{
    struct record req;
    struct record resp;
    const uint8_t *reply;
    uint16_t len;
    int handled;

    find_record(frames_path, request_name, &req);
    find_record(frames_path, response_name, &resp);
    handled = client_handle(&cl, req.body.data, req.body.len, crc16(req.body.data, req.body.len),
                            &reply, &len);
    CHECK(handled && len == resp.body.len && memcmp(reply, resp.body.data, len) == 0,
          "%s -> %s", request_name, response_name);
}

static void fixtures(void)
{
    /* HELLO at minor version 1, session 5A3C. */
    static const uint8_t hello[] = { 0x01, 0x3c, 0x5a, 0x00, 'S', 'P', 'R', 'K',
                                     1, 1, 0x00, 0x04 };
    static const uint8_t begun[] = { 0x90, 0x3c, 0x5a, 0x05, 0x00 };
    static const uint8_t ping[] = { 0x02, 0x3c, 0x5a, 0x06 };
    const uint8_t *reply;
    uint16_t len;
    struct fs_entry e;

    cl.max_payload = LIMIT;
    cl.capabilities = CAP_FILE_TREE | CAP_FILE_WRITE | CAP_FILE_REPLACE;
    cl.platform = PLATFORM_DOS;
    cl.version = "0.1.0";
    client_init(&cl, resp_buf);
    cl.minor = 1;
    cl.handler = tree_request;
    cl.reset = tree_reset;
    tree_init(dir, 1);

    client_handle(&cl, hello, sizeof hello, crc16(hello, sizeof hello), &reply, &len);
    CHECK(cl.session_minor == 1 && reply[0] == 0x81 && reply[9] == 1, "HELLO at minor 1");
    exchange("tree_stat", "tree_stat_reply");
    exchange("tree_file_read", "tree_file_read_reply");

    {
        struct record req;
        find_record(frames_path, "tree_file_begin", &req);
        client_handle(&cl, req.body.data, req.body.len, crc16(req.body.data, req.body.len),
                      &reply, &len);
        CHECK(len == sizeof begun && memcmp(reply, begun, len) == 0, "tree_file_begin -> found 0");
    }
    CHECK(data(0, "new") == 0, "FILE_DATA");
    CHECK(commit(3, crc_of("new")) == 4 && get32(out) == crc_of("new"), "FILE_COMMIT");
    CHECK(file_is("NEW.TXT", "new") && !exists("NEW.$$$"), "NEW.TXT written");
    CHECK(fs_stat(in_dir("NEW.TXT"), &e) == FS_OK && e.mtime == MTIME, "NEW.TXT has its time");
    /* A new request in between, so seq 5 is not answered from the cache. */
    client_handle(&cl, ping, sizeof ping, crc16(ping, sizeof ping), &reply, &len);
    exchange("tree_file_begin", "tree_error_exists");
}

static void lookups(void)
{
    tree_init(dir, 0);
    CHECK(stat_path("/") == 11 && out[1] == 1 && out[10] == 0, "the root is a directory");
    CHECK(stat_path("Games/doom.exe") == 11 + 14 && memcmp(out + 11, "GAMES/DOOM.EXE", 14) == 0,
          "found ignoring case, spelled as stored");
    CHECK(stat_path("/case/x.txt") > 0 && memcmp(out + 11, "case/x.txt", 10) == 0,
          "an exact match wins over one in another case");
    CHECK(stat_path("/case/X.txt") < 0 && class == ERR_AMBIGUOUS, "two matches are ambiguous");
    CHECK(stat_path("/nope") < 0 && class == ERR_NOT_FOUND, "missing");
    CHECK(stat_path("/A.TXT/x") < 0 && class == ERR_NOT_A_DIRECTORY, "below a file");
    CHECK(stat_path("/../etc") < 0 && class == ERR_NOT_FOUND, ".. refused");
    CHECK(stat_path("docs/./readme.txt") < 0 && class == ERR_NOT_FOUND, ". refused");
    CHECK(stat_path("docs\\readme.txt") < 0 && class == ERR_NOT_FOUND, "\\ refused");
    CHECK(stat_path("docs/") < 0 && class == ERR_NOT_FOUND, "a trailing / refused");
}

static void listing(void)
{
    static const uint8_t index0[2] = { 0, 0 };
    uint8_t index[2];
    uint16_t at = 0;
    int names = 0;
    int pages = 0;
    int len;
    int i;
    uint16_t p;

    tree_init(dir, 0);
    len = request(MSG_DIR_LIST, index0, 2, "/", LIMIT);
    CHECK(len > 2 && out[0] == 0 && out[1] == 5, "the root has 5 entries");
    CHECK(request(MSG_DIR_LIST, index0, 2, "A.TXT", LIMIT) < 0 && class == ERR_NOT_A_DIRECTORY,
          "listing a file");
    CHECK(request(MSG_FILE_READ, (const uint8_t *)"\0\0\0\0\0\4", 6, "docs", LIMIT) < 0
              && class == ERR_IS_A_DIRECTORY,
          "reading a directory");

    /* Room for one entry per page: the longest name is NEW.TXT. */
    do {
        index[0] = (uint8_t)at;
        index[1] = 0;
        len = request(MSG_DIR_LIST, index, 2, "", 2 + 11 + 7);
        if (len < 2 || out[1] == 0) {
            break;
        }
        for (i = 0, p = 2; i < out[1]; i++) {
            names++;
            p = (uint16_t)(p + 1 + out[p]);
        }
        at = (uint16_t)(at + out[1]);
        pages++;
    } while (out[0] & 1);
    CHECK(names == 5 && pages == 5, "paged listing: %d names in %d pages", names, pages);
}

static void writing(void)
{
    tree_init(dir, 0);
    CHECK(begin("X.TXT", 1, 0) < 0 && class == ERR_READ_ONLY, "read-only");

    tree_init(dir, 1);
    CHECK(begin("a.txt", 4, 0) < 0 && class == ERR_EXISTS, "no replace without the flag");
    CHECK(begin("docs", 4, 1) < 0 && class == ERR_IS_A_DIRECTORY, "a directory is not replaced");
    CHECK(begin("nope/X.TXT", 4, 0) < 0 && class == ERR_NOT_FOUND, "missing directory");
    CHECK(begin("/", 4, 0) < 0 && class == ERR_IS_A_DIRECTORY, "the root");
    CHECK(begin("a.txt", 4, 2) < 0 && class == ERR_INVALID_REQUEST, "unknown flag");

    CHECK(begin("a.txt", 4, 1) == 1 && out[0] == 1, "replace: found the destination");
    CHECK(exists("A.$$$"), "temporary file created");
    CHECK(data(1, "x") < 0 && class == ERR_INVALID_REQUEST, "data out of order");
    CHECK(data(0, "abcde") < 0 && class == ERR_INVALID_REQUEST, "data past the size");
    CHECK(data(0, "wx") == 0 && data(2, "yz") == 0, "data");
    CHECK(commit(4, crc_of("wxyz")) == 4, "commit");
    CHECK(file_is("A.TXT", "wxyz") && file_is("A.BAK", "abc") && !exists("A.$$$"),
          "replaced, old version kept");

    CHECK(begin("A.TXT", 2, 1) == 1 && out[0] == 3, "found the destination and a backup");
    CHECK(data(0, "ok") == 0 && commit(2, crc_of("ok")) == 4, "replace again");
    CHECK(file_is("A.TXT", "ok") && file_is("A.BAK", "wxyz"), "one backup generation");

    CHECK(begin("B.TXT", 2, 0) == 1 && data(0, "no") == 0, "begin B.TXT");
    CHECK(commit(2, crc_of("NO")) < 0 && class == ERR_VERIFY_FAILED, "CRC mismatch");
    CHECK(!exists("B.TXT") && !exists("B.$$$"), "nothing left after a failed check");
    CHECK(commit(2, crc_of("no")) < 0 && class == ERR_INVALID_REQUEST, "commit without a file");

    write_file("C.$$$", "old", 3, 0);
    CHECK(begin("C.TXT", 1, 0) == 1 && out[0] == 4, "leftover temporary file deleted");
    tree_reset();
    CHECK(!exists("C.$$$") && !exists("C.TXT"), "reset deletes the temporary file");
}

int main(int argc, char **argv)
{
    if (argc != 2) {
        printf("usage: test_tree VECTOR_DIR\n");
        return 2;
    }
    snprintf(frames_path, sizeof frames_path, "%s/frames.txt", argv[1]);
    make_tree();
    fixtures();
    lookups();
    listing();
    writing();
    remove_tree();
    printf("test_tree: %d failures\n", failures);
    return failures != 0;
}
