# What changes when you update

Mast has a new app identity. macOS may ask you again for permissions you use,
including Accessibility, Screen Recording, Automation and Notifications. Grant
the permissions you need when prompted. Previous Mast entries may remain in
System Settings.

If you use Linear, macOS may ask you to allow Mast to read its saved API key.
The key is copied to the Mast Keychain service; the existing item is retained.
Other agent credentials can also require renewed Keychain approval.

Your existing `~/.zeron` data stays in place and Mast continues to use it. New
installations use `~/.mast`. No data is moved by this update. Existing
`zeron://` conversation links continue to work; new links use `mast://`.
