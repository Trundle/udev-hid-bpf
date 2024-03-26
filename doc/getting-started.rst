.. _getting_started:

Getting started
===============

.. _dependencies:

Dependencies
------------

- ``rust``: install through your package manager or with ``rustup``::

   $ curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   $ source "$HOME/.cargo/env"

- ``udev`` and ``llvm``: Check the `.gitlab-ci.yml <https://gitlab.freedesktop.org/libevdev/udev-hid-bpf/-/blob/main/.gitlab-ci.yml>`_ for ``FEDORA_PACKAGES``.

.. _installation:

Installation
------------

Clone the repo, ``cd`` into it, and build the loader *and* the various example HID-BPF programs
using the standard Rust build process::

   $ git clone https://gitlab.freedesktop.org/libevdev/udev-hid-bpf.git
   $ cd udev-hid-bpf/
   $ cargo build

The above ``cargo`` command will build the tool and any BPF programs it finds in ``src/bpf/*.bpf.c``.
Please see the `cargo documentation <https://doc.rust-lang.org/cargo/>`_ for more details on invoking ``cargo``.

Then, we can install the binary with the following command::

   $ ./install.sh
   # this will ask for your sudo password to install udev rules and hwdb files

The above command will (re)build the tool and any BPF programs it finds in ``src/bpf/*.bpf.c``.
It will then install

- the tool itself into in ``/usr/local/bin``
- the compiled BPF objects in ``/usr/local/lib/firmware/hid/bpf``.
- a hwdb entry to tag matching devices in ``/etc/udev/hwdb.d/99-hid-bpf.hwdb``
- a udev rule to trigger the tool in ``/etc/udev/rules.d/99-hid-bpf.rules``

Running the BPF program
-----------------------

Once installed, unplug/replug any supported device, and the BPF program will automatically be attached to the HID kernel device.
