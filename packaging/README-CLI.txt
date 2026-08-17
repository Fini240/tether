Tether — command-line tool
==========================

This archive contains ONE FILE: the `tether` command-line program.

It is not an app. Double-clicking it does nothing useful — it needs a
subcommand, so Finder or Explorer will run it with no arguments, it will print
its usage, and it will quit.

If you wanted something to click, download the .dmg instead (macOS): that has
Tether.app, which lives in the menu bar and starts this same program for you.


Install
-------

macOS / Linux, from a terminal, in this folder:

    sudo install -m 0755 tether /usr/local/bin/tether

Windows: put tether.exe anywhere on your PATH.


macOS only: clear the download flag
-----------------------------------

macOS marks anything downloaded from the internet and refuses to run it,
blaming "an unidentified developer". That is the quarantine flag, not a
problem with the binary:

    xattr -dr com.apple.quarantine /usr/local/bin/tether

Then grant it permission to read your keyboard, without which nothing happens
and no error is shown:

    System Settings -> Privacy & Security -> Accessibility -> +
    and add /usr/local/bin/tether


Use it
------

On the machine with the keyboard and mouse:

    tether host --pair

On every other machine:

    tether client --pair

Both print a fingerprint. Check they match, then restart both without --pair —
from then on only those exact machines are accepted.

Then say where your screens actually are:

    tether layout                 # show the arrangement
    tether layout pc left mac     # the PC is to the left of the Mac

Check everything works on this machine:

    tether doctor


More
----

    tether help
    https://github.com/Fini240/tether
