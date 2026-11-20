# SPDX-License-Identifier: MIT
#
# make          build build/SPROCKET.EXE with Open Watcom and libc16 (8086, small model)
# make bench    build build/BENCH.EXE, which times the receive path on DOS
# make test     build and run the host tests against tests/vectors/ and tests/shell/
# make host     build build/sprocket-client, the same program for a Linux tty/PTY
# make check-version
#               check that SPROCKET.EXE's version is the release's (Cargo.toml)
# make check-map, check-map-negative
#               check the link maps: the programs' objects and libc16 only
# make check-rebuild
#               check that changing MAX_PAYLOAD rebuilds SPROCKET.EXE
# make dosbox-fmt
#               run the formatter's cases on DOS (DOSBox-X) against the host
# make clean    remove build/
# make help     print this list
#
# The DOS programs link libc16 (github.com/gr-dos/libc16), not Open Watcom's
# C runtime. By default fetch-libc16 fetches the pinned release into
# build/libc16, and its tools/fetch-watcom the Open Watcom snapshot it was
# built with into build/tools. LIBC16_ROOT=dir uses another libc16 tree (a
# release, or a checkout built with its make); WATCOM=dir another Open
# Watcom. MAX_PAYLOAD=n builds with a larger payload (up to 4090), for
# measuring disk imaging speed.

LIBC16_ROOT ?= build/libc16
# A release tree, or a checkout whose make stages the same layout in
# build/stage.
LIBC16_DIR := $(if $(wildcard $(LIBC16_ROOT)/build/stage/VERSION),$(LIBC16_ROOT)/build/stage,$(LIBC16_ROOT))
LIBC16 := $(LIBC16_DIR)/lib/LIBC16.LIB

ifeq ($(WATCOM),)
TOOLS_ENV := build/tools.env
WATCOM_ENV = . ./$(TOOLS_ENV) && export WATCOM && PATH="$$WATCOM/binl64:$$PATH"
else
TOOLS_ENV :=
WATCOM_ENV = WATCOM='$(WATCOM)' && export WATCOM && PATH="$$WATCOM/binl64:$$PATH"
endif

CC ?= cc
HOST_CFLAGS := -std=c99 -Wall -Wextra -pedantic -Werror -O2
ifdef MAX_PAYLOAD
HOST_CFLAGS += -DMAX_PAYLOAD=$(MAX_PAYLOAD)
endif

VECTORS := ../../tests/vectors
SHELL_FIXTURES := ../../tests/shell
CORE := src/frame.c src/proto.c
TREE := src/tree.c src/crc32.c
CLIENT := src/cli.c src/place.c src/req.c src/shell.c
HEADERS := $(wildcard src/*.h)

all: build/SPROCKET.EXE

build:
	mkdir -p build

# -0: 8086 instructions only; -ms: small model; -wx -we: all warnings, as
# errors. -ox optimizes for speed and -s drops the stack check on every
# call: together they cut BENCH's per-byte figure under 86Box (4.77 MHz
# 8088) from about 590 to 330 us (test/bench.c, in a version that also
# counted the frame check). Without the check the stack must be big enough
# by design: the only recursion is ls -R, at most 32 levels of a small
# frame, so the link's stack=8192 gives a stack of 8 KB, several times
# what is used.
#
# -zl -zld: no default library records in the objects; -x: no headers but
# include/ (<stdint.h>, which libc16 lacks) and libc16's; -fi: every object
# sees the declarations of printf and sprintf, which src/dosfmt.c provides
# because libc16 has no printf family. The link names no library but
# libc16 (option nodefaultlibs).
DOS_CFLAGS := -q -zq -zl -zld -x -bt=dos -ms -0 -ox -s -wx -we \
	-Iinclude -I$(LIBC16_DIR)/include -fi=src/dosfmt.h
ifdef MAX_PAYLOAD
DOS_CFLAGS += -dMAX_PAYLOAD=$(MAX_PAYLOAD)
endif
DOS_HEADERS := $(HEADERS) src/dosfmt.h include/stdint.h

# Make cannot tell that a variable given on the command line (MAX_PAYLOAD,
# LIBC16_ROOT, WATCOM) changed how a target is built, so each build's
# settings are recorded in a file, rewritten only when they differ from
# the last build's; the targets depend on it. Every make run checks it.
DOS_CONFIG := $(DOS_CFLAGS) | stack=8192 | $(LIBC16) | WATCOM=$(or $(WATCOM),$(TOOLS_ENV))
HOST_CONFIG := $(CC) $(HOST_CFLAGS)
config_stamp = printf '%s\n' '$(1)' | cmp -s - $@ || printf '%s\n' '$(1)' > $@

build/dos-config: FORCE | build
	@$(call config_stamp,$(DOS_CONFIG))

build/host-config: FORCE | build
	@$(call config_stamp,$(HOST_CONFIG))

DOS_SRC := $(CORE) $(TREE) $(CLIENT) src/storage.c src/biosdisk.c src/dosfs.c src/dosport.c \
	src/main.c src/dosfmt.c
BENCH_SRC := $(CORE) src/dosport.c src/dosfmt.c test/bench.c
HOST_SRC := $(CORE) $(TREE) $(CLIENT) src/storage.c src/imgdisk.c src/posixfs.c \
	src/posixport.c src/main.c

# $(call dos_compile,objdir,sources) and $(call dos_link,objdir,sources):
# every source is compiled again for each build, as the flags may differ
# (MAX_PAYLOAD), and $@ is linked with a map next to it.
dos_compile = $(WATCOM_ENV) && mkdir -p $(1) && for src in $(2); do \
	wcc $(DOS_CFLAGS) -fo=$(1)/$$(basename $$src .c).obj $$src || exit 1; done
dos_objs = $(foreach s,$(2),$(1)/$(basename $(notdir $(s))).obj)
dos_link = $(WATCOM_ENV) && wlink option quiet, nodefaultlibs, map=$(basename $@).map, stack=8192 \
	format dos name $@ $(addprefix file ,$(call dos_objs,$(1),$(2))) library $(LIBC16)

build/SPROCKET.EXE: $(DOS_SRC) $(DOS_HEADERS) $(LIBC16) $(TOOLS_ENV) build/dos-config | build
	$(call dos_compile,build/obj/SPROCKET,$(DOS_SRC))
	$(call dos_link,build/obj/SPROCKET,$(DOS_SRC))

build/BENCH.EXE: $(BENCH_SRC) $(DOS_HEADERS) $(LIBC16) $(TOOLS_ENV) build/dos-config | build
	$(call dos_compile,build/obj/BENCH,$(BENCH_SRC))
	$(call dos_link,build/obj/BENCH,$(BENCH_SRC))

# test/fmtcases.c with the DOS printf, for test/dosbox-fmt.sh.
FMTTEST_SRC := src/dosfmt.c test/fmtcases.c
build/FMTTEST.EXE: $(FMTTEST_SRC) $(DOS_HEADERS) $(LIBC16) $(TOOLS_ENV) build/dos-config | build
	$(call dos_compile,build/obj/FMTTEST,$(FMTTEST_SRC))
	$(call dos_link,build/obj/FMTTEST,$(FMTTEST_SRC))

# The pinned libc16 release. A LIBC16_ROOT given on the command line must
# already be built.
ifeq ($(LIBC16_ROOT),build/libc16)
$(LIBC16): fetch-libc16
	./fetch-libc16 $(LIBC16_ROOT)
	touch $@
endif

# The Open Watcom snapshot libc16 names, as a shell assignment.
build/tools.env: $(LIBC16) | build
	watcom=$$(sh $(LIBC16_DIR)/tools/fetch-watcom $(CURDIR)/build/tools) \
	  && printf 'WATCOM=%s\n' "$$watcom" > $@

bench: build/BENCH.EXE

# Each program's map must show only its own objects and members of the
# pinned libc16 (test/check-map), and neither may carry Open Watcom's
# runtime banner. check-map-negative relinks SPROCKET's objects with Open
# Watcom's own runtime, and with an extra object, and requires check-map
# to reject both.
check-map: build/SPROCKET.EXE build/BENCH.EXE
	test/check-map build/SPROCKET.map $(LIBC16) $(call dos_objs,build/obj/SPROCKET,$(DOS_SRC))
	test/check-map build/BENCH.map $(LIBC16) $(call dos_objs,build/obj/BENCH,$(BENCH_SRC))
	@if grep -l 'Open Watcom C/C++' build/SPROCKET.EXE build/BENCH.EXE; then \
	  echo "Open Watcom's runtime banner is in the files above" >&2; exit 1; fi

# Changing MAX_PAYLOAD must rebuild SPROCKET.EXE (build/dos-config).
check-rebuild:
	test/check-rebuild $(if $(WATCOM),WATCOM='$(WATCOM)') $(if $(filter command line,$(origin LIBC16_ROOT)),LIBC16_ROOT='$(LIBC16_ROOT)')

# The formatter's cases, on DOS under DOSBox-X and on the host, must print
# the same.
dosbox-fmt: build/FMTTEST.EXE build/test_fmtcases
	test/dosbox-fmt.sh

check-map-negative: build/SPROCKET.EXE
	$(WATCOM_ENV) && test/check-map-negative build/neg $(LIBC16) \
	  $(call dos_objs,build/obj/SPROCKET,$(DOS_SRC))

# SPROCKET.EXE reports VERSION from src/main.c; a release attaches it to the
# tag of Cargo.toml's version, so the two must agree.
check-version:
	@want=$$(sed -n 's/^version = "\(.*\)"$$/\1/p' ../../Cargo.toml | head -n 1); \
	have=$$(sed -n 's/^#define VERSION "\(.*\)"$$/\1/p' src/main.c); \
	if [ -z "$$want" ] || [ "$$want" != "$$have" ]; then \
	  echo "src/main.c VERSION \"$$have\" is not Cargo.toml's \"$$want\"" >&2; exit 1; \
	fi; \
	echo "SPROCKET.EXE version $$have"

build/test_frame: test/test_frame.c test/vectors.c src/frame.c $(HEADERS) build/host-config | build
	$(CC) $(HOST_CFLAGS) -o $@ test/test_frame.c test/vectors.c src/frame.c

build/test_proto: test/test_proto.c test/vectors.c $(CORE) $(HEADERS) build/host-config | build
	$(CC) $(HOST_CFLAGS) -o $@ test/test_proto.c test/vectors.c $(CORE)

build/test_tree: test/test_tree.c test/vectors.c $(CORE) $(TREE) src/place.c src/posixfs.c $(HEADERS) build/host-config | build
	$(CC) $(HOST_CFLAGS) -o $@ test/test_tree.c test/vectors.c $(CORE) $(TREE) src/place.c src/posixfs.c

build/test_shell: test/test_shell.c test/vectors.c src/shell.c $(HEADERS) build/host-config | build
	$(CC) $(HOST_CFLAGS) -o $@ test/test_shell.c test/vectors.c src/shell.c

build/test_storage: test/test_storage.c src/storage.c $(HEADERS) build/host-config | build
	$(CC) $(HOST_CFLAGS) -o $@ test/test_storage.c src/storage.c

build/sprocket-client: $(HOST_SRC) $(HEADERS) build/host-config | build
	$(CC) $(HOST_CFLAGS) -o $@ $(HOST_SRC)

host: build/sprocket-client

# The DOS build's printf (src/dosfmt.c), against the host's; no
# _FORTIFY_SOURCE, whose inline printf would clash with the renamed one.
build/test_dosfmt: test/test_dosfmt.c src/dosfmt.c src/dosfmt.h build/host-config | build
	$(CC) $(HOST_CFLAGS) -U_FORTIFY_SOURCE -D_FORTIFY_SOURCE=0 -o $@ test/test_dosfmt.c

# The same cases with the host C library's printf: what FMTTEST.EXE must print.
build/test_fmtcases: test/fmtcases.c build/host-config | build
	$(CC) $(HOST_CFLAGS) -o $@ test/fmtcases.c

test: build/test_frame build/test_proto build/test_tree build/test_shell build/test_storage \
	build/test_dosfmt
	build/test_frame $(VECTORS)
	build/test_proto $(VECTORS)
	build/test_tree $(VECTORS)
	build/test_shell $(SHELL_FIXTURES)
	build/test_storage
	build/test_dosfmt

clean:
	rm -rf build

# The target list at the top of this file.
help:
	@sed -n '3,/^#$$/s/^# //p' $(firstword $(MAKEFILE_LIST))

FORCE:

.PHONY: all bench check-map check-map-negative check-rebuild check-version dosbox-fmt host test \
	clean help FORCE
