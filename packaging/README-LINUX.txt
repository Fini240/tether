Tether for Linux
================

Two binaries:

    tether        the command-line version
    tether-gui    the window

Put "tether" somewhere on your PATH, e.g.

    sudo install -m 0755 tether /usr/local/bin/tether


One-time permission setup
-------------------------
Tether reads /dev/input/event* to capture your keyboard and mouse, and
writes /dev/uinput to let another machine drive this one. Both are
privileged. Grant them once:

    sudo cp 99-tether.rules /etc/udev/rules.d/
    sudo udevadm control --reload-rules && sudo udevadm trigger
    sudo modprobe uinput
    sudo usermod -aG input $USER

Then LOG OUT AND BACK IN — group membership only applies to new sessions,
and this is the step people skip before reporting that it does not work.

Check it took:

    tether doctor

Anyone in the "input" group can read every keystroke on this machine and
synthesise input as any user. That is what a software KVM is; the same is
true of Synergy, Barrier and Deskflow. Do not put accounts in that group
that you would not trust with a keylogger.


Use it
------
The same command on every machine — there is no host to nominate:

    tether run --pair

Compare the fingerprints it prints, then restart without --pair.


X11 or Wayland?
---------------
Either. Tether works below the display server, on the kernel's evdev and
uinput interfaces, so it does not care which one you are running, or
whether you are running one at all.

Two things that costs, both worth knowing:

  * Tether cannot ask where your cursor is — only the display server
    knows, and it will not say. Movement is tracked as deltas instead.
    On a single screen this makes no difference. Across two screens of
    different heights, the pointer can arrive at the wrong height when
    it crosses; correct it in the arrangement editor.

  * Screen positions come from the kernel, which knows the size of each
    display but not how you arranged them. One screen is exact. Several
    are guessed left-to-right in connector order, and may need dragging
    into place once.


The window
----------
tether-gui needs X11 or Wayland libraries present. The command-line
"tether" needs none of them and runs on a machine with no desktop at
all — which is the one to use over SSH or on a server.
