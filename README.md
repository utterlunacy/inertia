# inertia
inertia emulates the way trackpads work on the Steam Deck. After you swipe on the trackpad, the cursor will continue moving in the direction of the swipe and slowly decelerate. The exact behavior is configurable.

# how
inertia listens to your trackpad's events and analyzes the cursor movement to calculate the speed and direction of your cursor. It takes into account only the most recent movement (configurable time window) to make sure the direction is accurate and then moves the cursor pixel by pixel in the x and y direction in increasing intervals. Inertia uses grace periods to block unwanted cursor movement, e.g. after a multitouch gesture. To move the cursor, inertia registers a virtual mouse device. Due to permissions, inertia runs as root.

# features
- detailed configuration
- lightweight and stable
- keeps all trackpad functionality (e.g. multitouch gestures)

# install
- install cargo https://doc.rust-lang.org/cargo/getting-started/installation.html
  - i would recommend using your distro's package manager to install cargo, if applicable
- run `install.sh` as root. This will build inertia, move it to `/usr/local/bin`, set the default config in `/etc/inertia/config.toml`, and install the systemd service

# compatibility
- inertia has been extensively tested on CachyOS and Arch Linux. It should work fine on other distros, though
