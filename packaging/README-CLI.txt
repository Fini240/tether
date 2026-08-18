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

On every machine:

    tether run --pair

There is no host to nominate: the machines find each other and work out
between themselves which one arbitrates. Whichever keyboard you touch
drives, whichever way you push the pointer.

Each prints a fingerprint. Check they match, then restart them without
--pair — from then on only those exact machines are accepted.

If discovery cannot work — different subnets, or mDNS blocked — pin it
down by hand instead with "tether host" and "tether client --host IP".

Then say where your screens actually are:

    tether layout                 # show the arrangement
    tether layout pc left mac     # the PC is to the left of the Mac

Check everything works on this machine:

    tether doctor


More
----

    tether help
    https://github.com/Fini240/tether
