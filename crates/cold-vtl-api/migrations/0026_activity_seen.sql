-- When each account last opened the activity bell: newer finished operations are unread.
CREATE TABLE activity_seen (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES auth_users(id),
    seen_at TEXT NOT NULL
);
