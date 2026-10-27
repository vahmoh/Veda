// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only
//
// libthrower — a C++ shared library that verification/c/cxx.cc loads at
// run time: exceptions that leave it for the program, and the program's
// that pass through it, with the C++ library and the unwinder they share
// (libstdc++.so.6, libgcc_s.so.1), thread-local storage among them; its
// static objects constructed as it is loaded. verification/c/libraries.c
// loads it from C, which brings the C++ library in.

#include <exception>
#include <stdexcept>
#include <string>

namespace {

struct Registry {
	std::string name = "thrower";
};

Registry registry;

// The exceptions in flight, as the C++ library counted them, when one last
// passed through the library's frames (thrower_pass).
int in_flight_passing = -1;

}

extern "C" const char *thrower_name()
{
	return registry.name.c_str();
}

// Throws a std::runtime_error that says `n`, when `n` is not 0.
extern "C" void thrower_throw(int n)
{
	if (n)
		throw std::runtime_error("thrown in the library: " + std::to_string(n));
}

// Calls `f`, which throws, and catches what it throws, as the library's own
// `std::exception`: its message's length, or -1 if nothing came.
extern "C" int thrower_catch(void (*f)())
{
	try {
		f();
	} catch (const std::exception &e) {
		return static_cast<int>(std::string(e.what()).size());
	}
	return -1;
}

// Calls `f`, which throws, through the library's frames: the exception goes
// on to the program, which catches it, and what the library's frames hold
// is destroyed as it passes.
extern "C" void thrower_pass(void (*f)())
{
	struct Witness {
		~Witness() { in_flight_passing = std::uncaught_exceptions(); }
	} witness;
	std::string held = "destroyed as the exception passes";
	f();
	held.clear();
}

// The exceptions in flight as the C++ library counts them, now, and when
// one last passed through the library's frames: a process has one C++
// library, which counts the program's exceptions and the library's alike.
extern "C" int thrower_uncaught()
{
	return std::uncaught_exceptions();
}

extern "C" int thrower_uncaught_passing()
{
	return in_flight_passing;
}

// Throws an exception that says `n`, and catches it, inside the library:
// its message's length. What a C program that loads the library sees of
// its exceptions.
extern "C" int thrower_round_trip(int n)
{
	try {
		thrower_throw(n);
	} catch (const std::runtime_error &e) {
		return static_cast<int>(std::string(e.what()).size());
	}
	return -1;
}
