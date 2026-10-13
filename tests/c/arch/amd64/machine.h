/* What the test programs expect of the machine on AMD64. */
#ifndef VEDA_TESTS_MACHINE_H
#define VEDA_TESTS_MACHINE_H

/* The machine's name: what uname(2) and the auxiliary vector's
   AT_PLATFORM give. */
#define VEDA_MACHINE "x86_64"

/* The size of a page: what AT_PAGESZ and sysconf(_SC_PAGESIZE) give, and
   the unit of mmap, mprotect and madvise. */
#define VEDA_PAGE_SIZE 4096

#endif
