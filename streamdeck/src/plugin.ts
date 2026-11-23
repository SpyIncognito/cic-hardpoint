import streamDeck from '@elgato/streamdeck';
import { DownedAlert, FleetCount, GroupDial, Wingman } from './actions.ts';
import { defaultBusFile } from './busfile.ts';
import { FleetBus } from './client.ts';

const bus = new FleetBus({ busFile: defaultBusFile() });
for (const a of [new FleetCount(bus), new DownedAlert(bus), new Wingman(bus), new GroupDial(bus)]) {
  streamDeck.actions.registerAction(a);
}
bus.start();
void streamDeck.connect();
