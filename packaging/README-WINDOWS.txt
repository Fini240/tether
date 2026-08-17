Tether for Windows
==================

Two programs in this folder:

  Tether.exe       The app. Double-click this one.
  tether-cli.exe   The same thing without a window, for a terminal or a
                   service. (Not "tether.exe": Windows treats that as the
                   same name as Tether.exe, and one would overwrite the
                   other.)


Start here
----------

Double-click Tether.exe.

On the machine with the keyboard and mouse you want to share, tick
"Pair with a new machine" and press "Start as Host".
On every other machine, tick the same box and press "Start as Client".

Both windows show a fingerprint at the bottom right. Check they match, then
stop and start again with the pairing box unticked — from then on only those
exact machines are accepted.

Once they are connected, their screens appear in the window. Drag them so the
picture matches how they really sit on your desk. That is what decides which
screen edge leads where.


Windows will ask about the network
----------------------------------

The first time you start a host, Windows Defender Firewall asks whether to
allow it. Say yes for private networks. Without that, no other machine can
reach it.


Two things Windows will not let any program do
----------------------------------------------

  * Programs running as administrator cannot be driven by a program that is
    not. If Tether controls everything except, say, Task Manager, right-click
    Tether.exe and "Run as administrator".

  * The secure desktop -- UAC prompts, Ctrl+Alt+Del, and the lock screen --
    accepts no simulated input at all, from anything, at any privilege level.
    That is deliberate in Windows and cannot be worked around.


More
----

    tether-cli.exe help
    https://github.com/Fini240/tether
