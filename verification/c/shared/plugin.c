/* SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * libplugin — a shared library that verification/c/libraries.c loads at
 * run time: it needs libbase (DT_NEEDED, found beside it through its
 * RUNPATH of $ORIGIN), has functions, data and thread-local storage of
 * its own (which every thread of the program has, those started before it
 * was loaded too), a constructor that runs after libbase's, and a
 * destructor that runs as the program ends. */

#include <stdio.h>
#include <unistd.h>

extern int base_constructed;
int base_value(void);

int plugin_answer = 42;
int plugin_saw_base;
__thread int plugin_thread_calls;
__thread char plugin_thread_name[16] = "unset";

static void __attribute__((constructor)) construct(void)
{
	/* A dependency is constructed first. */
	plugin_saw_base = base_constructed;
}

static void __attribute__((destructor)) destruct(void)
{
	/* What a program loading it with "destroy" in its arguments looks
	   for, after its own end. */
	static const char gone[] = "plugin: destroyed\n";
	write(1, gone, sizeof gone - 1);
}

int plugin_twice(int x)
{
	return 2 * x;
}

int plugin_base(void)
{
	return base_value();
}

/* How many times the calling thread called it: its own count, which
   starts at 0 in every thread. */
int plugin_count(void)
{
	return ++plugin_thread_calls;
}

/* The calling thread's name, as it last set it (its storage's initial
   value, "unset", until then). */
const char *plugin_name(const char *set)
{
	if (set)
		snprintf(plugin_thread_name, sizeof plugin_thread_name, "%s", set);
	return plugin_thread_name;
}
