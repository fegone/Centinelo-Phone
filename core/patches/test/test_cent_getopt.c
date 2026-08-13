/*
 * Standalone unit test for core/deps/baresip/src/cent_getopt.h, the
 * MSVC-getopt-fallback added by
 * core/patches/0006-baresip-msvc-arg-parsing.patch.
 *
 * This is a pure-logic test: no baresip/re headers, no libraries, no
 * network, no build system wiring -- deliberately, so it can be run in
 * isolation to check the fallback parser's behavior before trusting a
 * full Windows build. It is NOT part of the CMake/ctest build (the
 * header it tests only exists in core/deps/baresip once the patch is
 * applied, and that submodule tree isn't this repo's own CMake
 * project) -- see core/patches/0006-baresip-msvc-arg-parsing.patch's
 * own doc comment for the exact command to compile and run this file.
 *
 * Every case here mirrors a real src/main.c optstring:
 * "46a:de:f:p:hu:n:vst:m:Tc" -- ':' after a letter means "takes a
 * value", matching what main() actually declares.
 */
#include <assert.h>
#include <stdio.h>
#include <string.h>

#include "../../deps/baresip/src/cent_getopt.h"

#define OPTSTRING "46a:de:f:p:hu:n:vst:m:Tc"

/* cent_optind/cent_optarg are file-scope state in cent_getopt.h, reset
 * between test cases by hand (see this header's own doc comment: no
 * built-in reset entry point, main.c only ever parses argv[] once).
 */
static void reset(void)
{
	cent_optind = 1;
	cent_optarg = NULL;
}

/* This is the exact invocation shell/src-tauri/src/sidecar.rs makes:
 * `cmd.arg("-f").arg(&plan.scratch_dir)` -- the one call site this
 * whole patch exists for.
 */
static void test_dash_f_separate_arg(void)
{
	char *argv[] = {"baresip", "-f", "C:\\Users\\test\\scratch", NULL};
	int argc = 3;

	reset();

	int c = cent_getopt(argc, argv, OPTSTRING);
	assert(c == 'f');
	assert(cent_optarg != NULL);
	assert(strcmp(cent_optarg, "C:\\Users\\test\\scratch") == 0);

	c = cent_getopt(argc, argv, OPTSTRING);
	assert(c == -1); /* no more options */

	printf("PASS test_dash_f_separate_arg\n");
}

/* "-fpath" -- value glued to the flag, no space. Not what sidecar.rs
 * sends today, but valid getopt() syntax main.c's real getopt() also
 * accepts, so the fallback must too.
 */
static void test_dash_f_glued_value(void)
{
	char *argv[] = {"baresip", "-f/tmp/scratch", NULL};
	int argc = 2;

	reset();

	int c = cent_getopt(argc, argv, OPTSTRING);
	assert(c == 'f');
	assert(strcmp(cent_optarg, "/tmp/scratch") == 0);

	printf("PASS test_dash_f_glued_value\n");
}

/* Bundled no-value flags, e.g. run-spike.sh's CENT_BARESIP_ARGS could
 * plausibly pass "-46" style debugging flags.
 */
static void test_bundled_flags(void)
{
	char *argv[] = {"baresip", "-46v", NULL};
	int argc = 2;

	reset();

	int c = cent_getopt(argc, argv, OPTSTRING);
	assert(c == '4');
	c = cent_getopt(argc, argv, OPTSTRING);
	assert(c == '6');
	c = cent_getopt(argc, argv, OPTSTRING);
	assert(c == 'v');
	c = cent_getopt(argc, argv, OPTSTRING);
	assert(c == -1);

	printf("PASS test_bundled_flags\n");
}

/* Unknown option must not hang, crash, or abort the whole parse --
 * matches real getopt(): returns '?' for that one arg and moves on
 * (main.c's own switch then hits its "case '?':" -> usage()+exit,
 * exactly like it does today on macOS/Linux for the same input).
 */
static void test_unknown_option_does_not_hang(void)
{
	char *argv[] = {"baresip", "-z", "-f", "/scratch", NULL};
	int argc = 4;

	reset();

	int c = cent_getopt(argc, argv, OPTSTRING);
	assert(c == '?');

	/* parsing must continue past the unknown flag, not get stuck */
	c = cent_getopt(argc, argv, OPTSTRING);
	assert(c == 'f');
	assert(strcmp(cent_optarg, "/scratch") == 0);

	printf("PASS test_unknown_option_does_not_hang\n");
}

/* Value-taking option with nothing after it: must return '?', not
 * read past the end of argv[] (this is the case ASan would catch if
 * the bounds check were wrong).
 */
static void test_missing_value_at_end(void)
{
	char *argv[] = {"baresip", "-f", NULL};
	int argc = 2;

	reset();

	int c = cent_getopt(argc, argv, OPTSTRING);
	assert(c == '?');

	printf("PASS test_missing_value_at_end\n");
}

/* No args at all, and a plain "--" terminator: both must return -1
 * without touching argv[cent_optind] out of bounds.
 */
static void test_no_args_and_terminator(void)
{
	char *argv1[] = {"baresip", NULL};
	int c = cent_getopt(1, argv1, OPTSTRING);
	assert(c == -1);

	char *argv2[] = {"baresip", "--", "-f", "/scratch", NULL};
	reset();
	c = cent_getopt(4, argv2, OPTSTRING);
	assert(c == -1);

	printf("PASS test_no_args_and_terminator\n");
}

/* A non-option positional argv[] entry (no leading '-') also just
 * ends parsing, same as real getopt() would once it hits one (main.c
 * doesn't use positional args, but the parser must not misread one as
 * an option or read out of bounds).
 */
static void test_positional_arg_stops_parsing(void)
{
	char *argv[] = {"baresip", "scratch_dir_without_flag", NULL};
	int argc = 2;

	reset();

	int c = cent_getopt(argc, argv, OPTSTRING);
	assert(c == -1);

	printf("PASS test_positional_arg_stops_parsing\n");
}

int main(void)
{
	test_dash_f_separate_arg();
	test_dash_f_glued_value();
	test_bundled_flags();
	test_unknown_option_does_not_hang();
	test_missing_value_at_end();
	test_no_args_and_terminator();
	test_positional_arg_stops_parsing();

	printf("all cent_getopt tests passed\n");
	return 0;
}
