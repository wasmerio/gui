// A program that waits for input for ever, and is killed while it does.
//
// A thread blocked in wguiInstanceWaitEvents is inside the host, where WASIX
// cannot reach it to deliver a signal. The host has to notice the kill itself
// and let go of the thread; otherwise this program never ends.

#include <pthread.h>
#include <signal.h>
#include <unistd.h>

#include "wgui_test.h"

static void* killer(void* unused) {
    (void)unused;
    usleep(100 * 1000);
    printf("killing\n");
    kill(getpid(), SIGKILL);
    return NULL;
}

int main(void) {
    WGUIInstance instance = 0;
    setvbuf(stdout, NULL, _IOLBF, 0);
    OK(wguiCreateInstance(NULL, &instance));
    pthread_t thread;
    CHECK(pthread_create(&thread, NULL, killer, NULL) == 0);
    printf("waiting\n");
    for (;;) {
        // No system call in this loop: only the host can end it.
        WGUIWaitReasons reasons = 0;
        if (wguiInstanceWaitEvents(instance, WGUI_WAIT_FOREVER, &reasons) != WGUIStatus_Success) {
            return 3;
        }
    }
}
