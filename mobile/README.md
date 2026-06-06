# Multica Mobile (iOS) — starter

An Expo + React Native starter app, set up so it can be built into an iOS
`.ipa` either on a Mac or in the cloud via EAS Build. Same stack Multica's own
mobile client uses (Expo / React Native).

> This is a scaffold, not the official Multica app. The agent list on screen is
> mock data — point it at the real Multica API to make it live.

## Tech stack

- Expo SDK 56 / React Native 0.85 / React 19
- New Architecture enabled
- EAS Build profiles in `eas.json` (`development`, `preview`, `production`)

## Run in development (no Mac needed)

```bash
cd mobile
npm install
npm start          # then press "i" or scan the QR with Expo Go on your iPhone
```

`npm start` serves JS over Metro. You can open it on a physical iPhone with the
**Expo Go** app — no Xcode required for this mode.

## Build an installable iOS app

A real `.ipa` requires Apple's toolchain. You have two options.

### Option A — Cloud build with EAS (no local Mac required)

EAS compiles on Expo's hosted macOS workers.

```bash
npm install -g eas-cli
eas login                       # needs a free Expo account
cd mobile
eas init                        # fills extra.eas.projectId in app.json
eas build --platform ios --profile preview
```

For an `.ipa` you can install on a real device, you need an **Apple Developer
Program** account ($99/yr) so EAS can manage signing credentials. The
`development` profile (`simulator: true`) builds a `.app` for the iOS Simulator
and does **not** require a paid Apple account.

Convenience scripts:

```bash
npm run build:ios:simulator     # simulator .app (free Apple account)
npm run build:ios:preview       # device .ipa, internal distribution
npm run build:ios:production    # store-ready build
```

### Option B — Local build on a Mac with Xcode

```bash
cd mobile
npm install
npx expo run:ios                # build & run on simulator / connected iPhone
```

Prerequisites: macOS, Xcode, CocoaPods, and (for a physical device) an Apple ID
added to Xcode with Developer Mode enabled on the iPhone.

## Before you ship

- Change `ios.bundleIdentifier` / `android.package` in `app.json` from
  `com.example.multicamobile` to an identifier you own.
- Replace the placeholder icons in `assets/`.
