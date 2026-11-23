# FAQ

**Does Hardpoint press keys or touch the game's files?**
No. Hardpoint is API only: it reads the game's local telemetry API on `127.0.0.1:2942`, and its
buttons call that same API. It changes no game files.

**Does it run on Linux or macOS?**
The app with its windows is for Windows. The fleet server, CIC, runs on Linux.

**Do I need a CIC server to fly with friends?**
No. One of you can host a peer-to-peer fleet from Hardpoint itself; see
[Peer to peer](Fleet-P2P.md). CIC is for orgs that plan events with units and roles ahead of
time.

**What do other players see about me?**
Only the fields you turn on under **What I send**, and only the people your seat or the fleet
allows. Everything is off until you turn it on. Your aUEC balance, keybinds and sequences are
never on a card. See [What you share](Fleet-CIC.md#what-you-share).

**Does a peer-to-peer host see my card?**
Yes. The host reads every card that passes through their PC, and every member sees every card.
Each sees only the fields you send. The join screen says so before you join.

**Does the CIC server store my card?**
No. CIC passes cards on to the members who may see them and keeps them in memory only. It keeps
your account, your events and the event's units and roles.

**What is my fleet key?**
A key Hardpoint makes the first time you use a fleet, kept in `fleet-identity.key` in the
settings folder. A peer-to-peer fleet knows you by it, so a rejoin and a ban follow the key, not
your handle; CIC links it to your account when you connect. Keep the file private. The panel
shows its fingerprint.

**I changed PC. How do I take my setup with me?**
Copy the settings folder, `%APPDATA%\Hardpoint`, while Hardpoint is closed. To move dashboards
only, use **Export** and **Import**. For a CIC event, just make a new connection code on the new
PC.

**Can I use Hardpoint on a laptop next to my game PC?**
Yes, in companion mode: the laptop shows the game PC's dashboards and fleet panel. Both run the
same Hardpoint version. See [Companion](Companion.md).

**Why does my laptop's Stream Deck read NO FLEET?**
A companion has no fleet of its own. Plug the deck into the game PC and install the plugin there.

**Can I change seats, roles or units from Hardpoint?**
No, that is done in the CIC portal. Use **Open in CIC** in the fleet panel to go to the event's
page.

**How many people can join a peer-to-peer fleet?**
Up to 64, the host included.

**Does Hardpoint set up my router for hosting?**
No. Forward TCP port 47522 (or the port you chose) by hand, or use a VPN.

**How do I turn off the launch splash?**
Set `"splash": false` in `app-settings.json`, or start with `hardpoint.exe --no-splash`.

**Where do I report a problem?**
On the project's issue tracker. See [Troubleshooting](Troubleshooting.md#reporting-a-bug) for
what to include.
