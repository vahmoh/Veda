// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only
//
// cxx — checks of the C++ library (libstdc++) on Veda, which programs in
// C++ build on: exceptions, run-time types, static constructors, containers
// and strings, streams, threads; and, linked dynamically, exceptions
// between it and a C++ library it loads (verification/c/shared/thrower.cc),
// which share the C++ library and the unwinder (libstdc++.so.6,
// libgcc_s.so.1).
//
// Each check prints "ok - NAME" or "not ok - NAME"; the program exits with
// the number of failed checks (0: all passed). `systest` runs it inside
// Veda in a scratch directory.

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <dlfcn.h>
#include <exception>
#include <fstream>
#include <iostream>
#include <map>
#include <memory>
#include <mutex>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <typeinfo>
#include <vector>

static int failures;

static void check(const char *name, bool ok)
{
	std::printf("%s - %s\n", ok ? "ok" : "not ok", name);
	if (!ok)
		failures++;
}

// Constructed before main runs.
struct Global {
	int value = 42;
	std::string name = "global";
};
static Global global;

struct Base {
	virtual ~Base() = default;
	virtual int kind() const { return 1; }
};

struct Derived : Base {
	int kind() const override { return 2; }
};

thread_local int per_thread = 1;

static void check_exceptions()
{
	bool caught = false;
	try {
		std::vector<int> v(3);
		(void)v.at(10);
	} catch (const std::out_of_range &) {
		caught = true;
	}
	check("exception from the library", caught);
	caught = false;
	try {
		throw 7;
	} catch (int x) {
		caught = x == 7;
	}
	check("exception of a plain type", caught);

	// One thread throws, another rethrows what it caught.
	std::exception_ptr error;
	std::thread([&] {
		try {
			throw std::runtime_error("from a thread");
		} catch (...) {
			error = std::current_exception();
		}
	}).join();
	caught = false;
	try {
		if (error)
			std::rethrow_exception(error);
	} catch (const std::runtime_error &e) {
		caught = std::string(e.what()) == "from a thread";
	}
	check("exception carried across threads", caught);
}

static void check_types_and_containers()
{
	check("static constructor", global.value == 42 && global.name == "global");
	std::unique_ptr<Base> b = std::make_unique<Derived>();
	check("virtual call, dynamic_cast, typeid",
	      b->kind() == 2 && dynamic_cast<Derived *>(b.get()) && typeid(*b) == typeid(Derived));
	std::map<std::string, int> m{{"one", 1}, {"two", 2}};
	std::string s = "veda";
	s += std::to_string(m["two"]);
	check("containers and strings", s == "veda2" && m.size() == 2);
}

static void check_streams()
{
	std::ostringstream os;
	os << "x=" << 3.5 << ' ' << std::hex << 255;
	check("string stream", os.str() == "x=3.5 ff");
	{
		std::ofstream out("cxx.txt");
		out << "first line\n";
	}
	std::ifstream in("cxx.txt");
	std::string line;
	std::getline(in, line);
	check("file streams", line == "first line");
	std::remove("cxx.txt");
}

static void check_threads()
{
	std::mutex mu;
	std::condition_variable cv;
	int ready = 0;
	std::atomic<int> sum{0};
	std::vector<std::thread> threads;
	for (int i = 0; i < 4; i++)
		threads.emplace_back([&, i] {
			per_thread = 100 + i;
			sum += per_thread;
			std::lock_guard<std::mutex> lock(mu);
			ready++;
			cv.notify_one();
		});
	{
		std::unique_lock<std::mutex> lock(mu);
		cv.wait(lock, [&] { return ready == 4; });
	}
	for (auto &t : threads)
		t.join();
	check("threads, a mutex, a condition variable", sum == 406);
	check("thread_local", per_thread == 1);

	auto start = std::chrono::steady_clock::now();
	std::this_thread::sleep_for(std::chrono::milliseconds(20));
	auto ms = std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::steady_clock::now() - start);
	check("clocks and sleeping", ms.count() >= 20 && ms.count() < 5000);
}

// What the program gives libthrower to call: it throws.
static void program_throws()
{
	throw std::logic_error("thrown in the program");
}

// Exceptions between the program and a C++ library it loads, which share
// the C++ library and the unwinder: thrown in the library and caught in
// the program, thrown in the program and caught in the library, and thrown
// in the program through the library's frames back to it, the C++
// library counting each once. Linked statically, the program loads
// nothing.
static void check_shared_library()
{
	void *h = dlopen("/system/tests/c/lib/libthrower.so", RTLD_NOW);
	if (!h) {
		const char *e = dlerror();
		check("loading libthrower (only linked dynamically)", e && std::strstr(e, "linked statically"));
		return;
	}
	auto name = reinterpret_cast<const char *(*)()>(dlsym(h, "thrower_name"));
	auto throws = reinterpret_cast<void (*)(int)>(dlsym(h, "thrower_throw"));
	auto catches = reinterpret_cast<int (*)(void (*)())>(dlsym(h, "thrower_catch"));
	auto passes = reinterpret_cast<void (*)(void (*)())>(dlsym(h, "thrower_pass"));
	auto uncaught = reinterpret_cast<int (*)()>(dlsym(h, "thrower_uncaught"));
	auto uncaught_passing = reinterpret_cast<int (*)()>(dlsym(h, "thrower_uncaught_passing"));
	if (!name || !throws || !catches || !passes || !uncaught || !uncaught_passing) {
		check("libthrower's functions", false);
		return;
	}
	// The program's C++ library is the system's shared one.
	Dl_info info{};
	int (*counts)() noexcept = std::uncaught_exceptions;
	check("the C++ library, shared",
	      dladdr(reinterpret_cast<void *>(counts), &info) && info.dli_fname &&
		      std::strcmp(info.dli_fname, "/system/lib/libstdc++.so.6") == 0);
	check("a library's static objects", std::string(name()) == "thrower");
	std::string message;
	try {
		throws(42);
	} catch (const std::runtime_error &e) {
		message = e.what();
	}
	check("an exception from a library", message == "thrown in the library: 42");
	check("an exception from a library, caught and counted once", std::uncaught_exceptions() == 0 && uncaught() == 0);
	check("an exception caught in a library", catches(program_throws) == int(std::strlen("thrown in the program")));
	bool through = false;
	try {
		passes(program_throws);
	} catch (const std::logic_error &) {
		through = true;
	}
	check("an exception through a library's frames", through);
	check("an exception in flight through a library's frames, counted there", uncaught_passing() == 1);
	// Its thread-local exception state is each thread's.
	int in_threads = 0;
	std::vector<std::thread> threads;
	std::mutex mu;
	for (int i = 1; i <= 4; i++)
		threads.emplace_back([&, i] {
			try {
				throws(i);
			} catch (const std::exception &) {
				std::lock_guard<std::mutex> lock(mu);
				in_threads++;
			}
		});
	for (auto &t : threads)
		t.join();
	check("exceptions from a library in threads", in_threads == 4);
}

int main()
{
	check_exceptions();
	check_types_and_containers();
	check_streams();
	check_threads();
	check_shared_library();
	std::cout << (failures ? "FAIL" : "PASS") << ": " << failures << " failed" << std::endl;
	return failures;
}
