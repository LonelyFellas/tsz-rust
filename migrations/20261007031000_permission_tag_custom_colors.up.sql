ALTER TABLE permission_tags DROP CONSTRAINT permission_tags_color_check;
ALTER TABLE permission_tags ADD CONSTRAINT permission_tags_color_check
    CHECK (color IN ('default', 'blue', 'cyan', 'green', 'gold', 'orange', 'red', 'purple')
        OR color ~ '^#[0-9A-Fa-f]{6}$');
