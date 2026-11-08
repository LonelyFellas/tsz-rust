ALTER TABLE users ADD COLUMN interface_locale TEXT
    CONSTRAINT users_interface_locale_check CHECK (interface_locale IN ('zh-Hans', 'zh-Hant', 'en-GB', 'en-US'));
ALTER TABLE admins ADD COLUMN interface_locale TEXT
    CONSTRAINT admins_interface_locale_check CHECK (interface_locale IN ('zh-Hans', 'zh-Hant', 'en-GB', 'en-US'));
