import streamDeck, { action, SingletonAction, type DialDownEvent, type DialRotateEvent, type DidReceiveSettingsEvent, type KeyDownEvent, type PropertyInspectorDidAppearEvent, type TouchTapEvent, type WillAppearEvent, type WillDisappearEvent } from '@elgato/streamdeck';
import type { FleetBus } from './client.ts';
import type { Connection } from './protocol.ts';
import { keyImage, stripImage } from './render.ts';
import {
  acknowledge, dialScopes, dialView, downedAlert, fleetCountView, lists, NEW_ALERT, online, stepDial, wingmanView,
  type AlertSettings, type AlertState, type CountSettings, type DialSettings, type KeyView, type WingmanSettings,
} from './views.ts';

type JsonValue = Parameters<typeof streamDeck.ui.sendToPropertyInspector>[0];

type AnyAction = WillAppearEvent['action'];

const FLASH_MS = 500;

/** One kind of key: keeps each visible key's settings and redraws them all on every change. */
abstract class FleetAction<S> extends SingletonAction {
  protected readonly bus: FleetBus;
  protected readonly settings = new Map<string, S>();

  constructor(bus: FleetBus) {
    super();
    this.bus = bus;
    bus.onChange(() => this.redrawAll());
  }

  protected abstract draw(a: AnyAction, settings: S, c: Connection): Promise<void> | void;

  protected redraw(a: AnyAction): void {
    void this.draw(a, this.settings.get(a.id) ?? ({} as S), this.bus.connection);
  }

  redrawAll(): void {
    for (const a of this.actions) this.redraw(a);
    if (streamDeck.ui.action && this.settings.has(streamDeck.ui.action.id)) void this.sendLists();
  }

  protected sendLists(): Promise<void> {
    return streamDeck.ui.sendToPropertyInspector(lists(this.bus.connection) as unknown as JsonValue);
  }

  override onWillAppear(ev: WillAppearEvent): void {
    this.settings.set(ev.action.id, ev.payload.settings as S);
    this.redraw(ev.action);
  }

  override onWillDisappear(ev: WillDisappearEvent): void {
    this.settings.delete(ev.action.id);
  }

  override onDidReceiveSettings(ev: DidReceiveSettingsEvent): void {
    this.settings.set(ev.action.id, ev.payload.settings as S);
    this.redraw(ev.action);
  }

  override onPropertyInspectorDidAppear(_ev: PropertyInspectorDidAppearEvent): Promise<void> {
    return this.sendLists();
  }

  override onSendToPlugin(_ev: Parameters<NonNullable<SingletonAction['onSendToPlugin']>>[0]): Promise<void> {
    return this.sendLists();
  }
}

function showKey(a: AnyAction, view: KeyView, lit = true): Promise<void> | void {
  if (a.isKey()) return a.setImage(keyImage(view, lit));
}

@action({ UUID: 'games.spyincognito.hardpoint.fleet.count' })
export class FleetCount extends FleetAction<CountSettings> {
  protected draw(a: AnyAction, s: CountSettings, c: Connection) {
    return showKey(a, fleetCountView(c, s));
  }

  override async onKeyDown(ev: KeyDownEvent): Promise<void> {
    if (this.bus.refresh()) await ev.action.showOk();
  }
}

@action({ UUID: 'games.spyincognito.hardpoint.fleet.downed' })
export class DownedAlert extends FleetAction<AlertSettings> {
  readonly #alerts = new Map<string, AlertState>();
  readonly #flashing = new Set<string>();
  #timer: ReturnType<typeof setInterval> | undefined;
  #lit = true;

  protected draw(a: AnyAction, s: AlertSettings, c: Connection) {
    const { view, state } = downedAlert(c, s, this.#alerts.get(a.id) ?? NEW_ALERT);
    this.#alerts.set(a.id, state);
    this.#flash(a.id, view.flash === true);
    return showKey(a, view, this.#lit);
  }

  #flash(id: string, on: boolean): void {
    if (on) this.#flashing.add(id);
    else this.#flashing.delete(id);
    if (this.#flashing.size > 0 && !this.#timer) {
      this.#timer = setInterval(() => {
        this.#lit = !this.#lit;
        for (const a of this.actions) if (this.#flashing.has(a.id)) this.redraw(a);
      }, FLASH_MS);
    } else if (this.#flashing.size === 0 && this.#timer) {
      clearInterval(this.#timer);
      this.#timer = undefined;
      this.#lit = true;
    }
  }

  override onWillDisappear(ev: WillDisappearEvent): void {
    super.onWillDisappear(ev);
    this.#alerts.delete(ev.action.id);
    this.#flash(ev.action.id, false);
  }

  override onKeyDown(ev: KeyDownEvent): void {
    this.#alerts.set(ev.action.id, acknowledge(this.#alerts.get(ev.action.id) ?? NEW_ALERT));
    this.redraw(ev.action);
  }
}

@action({ UUID: 'games.spyincognito.hardpoint.fleet.wingman' })
export class Wingman extends FleetAction<WingmanSettings> {
  protected draw(a: AnyAction, s: WingmanSettings, c: Connection) {
    return showKey(a, wingmanView(c, s));
  }
}

@action({ UUID: 'games.spyincognito.hardpoint.fleet.dial' })
export class GroupDial extends FleetAction<DialSettings> {
  readonly #current = new Map<string, string>();

  protected draw(a: AnyAction, s: DialSettings, c: Connection) {
    if (a.isDial()) return a.setFeedback({ canvas: stripImage(dialView(c, s, this.#current.get(a.id) ?? 'fleet')) });
  }

  override onDialRotate(ev: DialRotateEvent): void {
    const c = this.bus.connection;
    const status = online(c);
    if (!('scopes' in status)) return;
    const list = dialScopes(status, (this.settings.get(ev.action.id) ?? {}) as DialSettings);
    this.#current.set(ev.action.id, stepDial(list, this.#current.get(ev.action.id) ?? 'fleet', ev.payload.ticks));
    this.redraw(ev.action);
  }

  override onDialDown(ev: DialDownEvent): void {
    this.#home(ev.action);
  }

  override onTouchTap(ev: TouchTapEvent): void {
    this.#home(ev.action);
  }

  #home(a: AnyAction): void {
    this.#current.set(a.id, 'fleet');
    this.redraw(a);
  }

  override onWillDisappear(ev: WillDisappearEvent): void {
    super.onWillDisappear(ev);
    this.#current.delete(ev.action.id);
  }
}
