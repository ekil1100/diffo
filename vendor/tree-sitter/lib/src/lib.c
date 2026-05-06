// Enable GNU/POSIX extensions for -std=c11 compatibility
#define _GNU_SOURCE
#define _POSIX_C_SOURCE 200809L

#include <stdio.h>
#include <endian.h>

#include "./alloc.c"
#include "./get_changed_ranges.c"
#include "./language.c"
#include "./lexer.c"
#include "./node.c"
#include "./parser.c"
#include "./point.c"
#include "./query.c"
#include "./stack.c"
#include "./subtree.c"
#include "./tree_cursor.c"
#include "./tree.c"
#include "./wasm_store.c"
