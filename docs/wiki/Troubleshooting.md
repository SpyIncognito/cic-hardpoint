# Troubleshooting

Start with what Hardpoint says: most messages name the cause, and a refusal adds what to do.

## Install and start

**Windows SmartScreen warns about hardpoint.exe.** The exe is not code-signed yet. Check its
SHA-256 against `SHA256SUMS` from the release ([Install and set up](Install-and-setup.md#download-and-check)),
then choose **More info** and **Run anyway**.

**Starting Hardpoint again does nothing new.** Only one Hardpoint runs at a time; a second start
brings the running one's dashboard to the front. If you closed the window, Hardpoint is still in
the tray: use **Open dashboard** there.

**The status strip says Waiting for the game.** Hardpoint has not heard from the game's telemetry
API on `127.0.0.1:2942` yet. Start the game (or wait for it to finish loading) on the same PC, and
check the API is switched on for your game version ([Turn on the game's telemetry
API](Install-and-setup.md#turn-on-the-games-telemetry-api)). On 4.x LIVE, set the variable and
then fully quit and restart the RSI Launcher, not only the game: the game takes its environment
from the launcher. When the API is on, `Game.log` shows `127.0.0.1:2942 [Insecure]` (4.x LIVE) or
`Local services server is running on 127.0.0.1:2942` (5.0 PTU).

**Action buttons do nothing on LIVE.** Game 4.x serves the telemetry API display only; actions
need game 5.0 (PTU now).

**Widgets are dimmed with NOT ON SHIP or N/A.** Your ship does not report that function. Hover
for the reason. This is expected; see [Dashboards](Dashboards.md#ship-aware-dashboards). If a
ship was learned wrong, use **Reset for every ship** under **Functions a ship lacks**.

## Dashboards

**My dashboard is gone after going back to an older build.** Builds from before ship dashboards
write over the newer file. Put `dashboards.json.bak` from the settings folder back as
`dashboards.json`, or import the export you made.

**The dashboard says the file is from a newer version.** A newer Hardpoint wrote it. This build
does not read or write it; your changes stay in memory only until you update Hardpoint.

**A detached tab does not open on its display.** That display is not connected, or it has no name
(only displays with a name can be remembered), or two displays share a name. Detach again and
choose the display.

**An import left things out.** The summary lists each item and why: an unknown widget, a theme
this Hardpoint does not have, a bad bezel mapping and so on. The rest is kept.

## Fleet: general

**The panel says Fleet offline, trying again in N s.** The connection dropped. Hardpoint
reconnects by itself (1 s, doubling to 30 s). If it keeps failing, check your network, then ask
whoever runs the server or hosts the fleet.

**Everyone shows OFFLINE.** A card that has not been updated for 10 s shows OFFLINE. If it is
everyone, your own connection is probably down: see the line under the panel's header.

**"fleet protocol N is not supported: this side needs protocol 2.x".** Hardpoint and the server
speak different major versions of the fleet protocol. Update Hardpoint, or ask the server's
operator (or the host) to update theirs.

**"Sharing off" in the header.** You send no fields yet. Press **What I send** and turn on what you
want to share.

### CIC refusals

| Hardpoint says | Why | What to do |
|---|---|---|
| ...then "Make a new connection code in the CIC portal" | The code was used already, is past its 12 hours, is not known, or this Hardpoint is not linked to an account yet | On the portal's **Account** page press **Make a connection code** and paste the new one |
| ...then "The account is disabled: ask a CIC admin" | Your account was revoked | Ask a CIC admin |
| Kicked | You were kicked from the event | Talk to the event's admiral |
| The event is not offered, or it is another event | You chose an event that is not in the list, or the code was made for another event | Paste the code again and choose from the list |

You made a new connection code on another PC? A new code links that PC and drops the old one.
Each account has one Hardpoint at a time.

### Peer-to-peer refusals

| Hardpoint adds | Why | What to do |
|---|---|---|
| "Ask the host for the current invite" | A wrong or old code, or too many wrong codes from your address | Ask the host for the invite they show now |
| "The host banned this fleet key: only the host can lift it" | The host kicked you | Only the host can lift it |
| "The fleet is full: ask the host to make room" | The fleet has 64 members | Ask the host |
| "The host's session ended: ask the host for a new invite once they host again" | The host restarted Hardpoint or stopped hosting | Get a new invite once they host again |

Removed by the host (not kicked)? Join again with the current invite.

### Hosting

**Friends on your network cannot join.** Check that Windows allowed Hardpoint on private
networks when you started hosting (see [Firewall](#firewall)), and that your network is set as
private in Windows.

**Friends outside your network cannot join.** Forward TCP port 47522 (or your port) on your
router to the hosting PC, type your public address or DNS name in the Hosting panel, press
**Use this address**, and send them the public invite. Hardpoint does not open router ports for
you. Or use a VPN.

**The Host button is missing.** Hosting runs on the game PC itself; a companion's page does not
offer it.

## Companion

| Message | Why | What to do |
|---|---|---|
| "this game PC runs Hardpoint A and the companion B; update both to the same version" | The two machines run different Hardpoint versions | Install the same release on both |
| "pairing failed: check the code" | A wrong code (it does not say which side typed it wrong) | Check the address and the 8 digits, and try again |
| "pairing stopped after 3 wrong codes" | Three wrong codes | Press **New code** on the companion |
| "the companion is not showing a pairing code" | The code expired (2 minutes) or was never shown | Press **Show pairing code** on the companion |
| "too many pairing attempts; wait 5 s" | One try per address every 5 s | Wait, then try again |
| "pairing was cancelled on the companion" | **Cancel** was pressed on the companion, or the code ran out | Start again |
| "not paired at <address>" | Another machine answers at that address now | Check the address, or pair again |

**The game PC keeps saying connecting to <name>.** The laptop is asleep, off the network, or not
in **Be a companion** mode, or its firewall blocks TCP 47521. Wake it and check the mode.

**Buttons on the companion answer "stale".** The action arrived more than 1 s after you pressed
it: the network between the two is slow. Try a wired connection or a closer Wi-Fi access point.

## Stream Deck

**Keys read HARDPOINT OFF.** Hardpoint is not running on this PC, or the plugin cannot find
`%APPDATA%\sc-key-emulator\bus.json`. Start Hardpoint; the plugin tries again every 2 s. Do not
delete the `sc-key-emulator` folder.

**Keys read NO FLEET.** Join a fleet in Hardpoint. On a companion the keys always read NO FLEET:
install the plugin on the game PC.

**A key reads NO ACCESS.** Your role does not cover that scope or member. Pick another in the
key's settings.

## Firewall

Windows asks only when Hardpoint listens for other machines: when you host (TCP 47522) and when
a laptop is a companion (TCP 47521). Allow private networks only. To fix a wrong choice, open
**Windows Security**, **Firewall & network protection**, **Allow an app through firewall**, and
change the entry for Hardpoint.

## Do I need a hard refresh?

No. Hardpoint's pages come from the app itself, fresh every time, so there is no cached copy to
clear after an update. On the CIC side, each update gets new asset addresses, and the CIC Live
page offers **Reload** when the server runs a newer build.

**The Fleet tab shows an error with Reload.** Something stopped the board. Press **Reload**. If
it comes back, note the message for a bug report.

## Reporting a bug

Say what you did, what you expected and what Hardpoint said. A telemetry recording (tray:
**Record telemetry**) helps with dashboard problems; recordings are scrubbed of your aUEC balance
and ship, seat and weapon names.
