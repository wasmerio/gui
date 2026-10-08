// A program that asks for events, or waits for them, and enters the system
// nowhere else: what a loop that only draws is like. WASIX delivers signals
// where a program enters the system, so one sent to this program would never
// arrive. The host lets it in where the program asks and waits.
//
//     signals poll    asks again and again
//     signals wait    waits a hundredth of a second at a time
//     signals sleep   waits for as long as it takes

#include <pthread.h>
#include <signal.h>
#include <unistd.h>

#include "wgui_test.h"

static volatile sig_atomic_t signalled;

static void on_signal(int number) {
    signalled = number;
}

static void* sender(void* unused) {
    (void)unused;
    usleep(100 * 1000);
    kill(getpid(), SIGUSR1);
    return NULL;
}

int main(int argc, char** argv) {
    const char* mode = argc > 1 ? argv[1] : "poll";
    WGUIInstance instance = 0;
    setvbuf(stdout, NULL, _IOLBF, 0);
    OK(wguiCreateInstance(NULL, &instance));
    CHECK(signal(SIGUSR1, on_signal) != SIG_ERR);
    pthread_t thread;
    CHECK(pthread_create(&thread, NULL, sender, NULL) == 0);
    printf("in a loop that only %ss\n", mode);

    static uint64_t buffer[WGUI_MAX_EVENT_SIZE / 8];
    unsigned interrupted = 0;
    // No system call in this loop: the handler runs inside the host's calls.
    while (!signalled) {
        if (strcmp(mode, "poll") == 0) {
            size_t written = 0;
            OK(wguiInstancePollEvents(instance, buffer, sizeof buffer, &written));
        } else {
            uint64_t timeout = strcmp(mode, "wait") == 0 ? 10 * 1000 * 1000 : WGUI_WAIT_FOREVER;
            WGUIWaitReasons reasons = 0;
            OK(wguiInstanceWaitEvents(instance, timeout, &reasons));
            interrupted += (reasons & WGUIWaitReasons_Interrupted) != 0;
        }
    }
    printf("%s reached it\n", signalled == SIGUSR1 ? "SIGUSR1" : "another signal");
    // A long sleep is cut short for this, and a short one never is.
    if (strcmp(mode, "sleep") == 0) {
        CHECK(interrupted == 1);
    } else {
        CHECK(interrupted == 0);
    }
    CHECK(pthread_join(thread, NULL) == 0);
    wguiInstanceDestroy(instance);
    printf("done\n");
    return 0;
}
