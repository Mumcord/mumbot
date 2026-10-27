"""Prints SQL that seeds the load-test server with ~300 nested channels (up to 5 levels deep).

Carried over from Mumcord's testenv/scale/, where the client's mock API mirrors the same tree.
"""

GAMES = ["Starfall", "Iron Tide", "Neon Drift", "Hollow Crown", "Skyforge", "Deep Rust",
         "Ember Line", "Frost Hollow", "Void Runner", "Tidebreak", "Cinder Peak", "Rogue Signal"]
CLANS = ["Night Owls", "Red Foxes", "Blue Herons", "Grey Wolves", "Gold Lions", "Black Cats",
         "White Ravens", "Green Vipers", "Silver Hawks", "Iron Bears", "Storm Crows",
         "Sand Eagles", "Moon Moths", "Sun Bees", "Fire Ants"]
TOPICS = ["Tech Talk", "Movies & TV", "Books", "Art & Design", "Cooking", "Sports", "Travel",
          "Pets"]


def layout():
    """(id, parent_id, name) rows in creation order; root (0) is not included."""
    rows = []

    def add(parent, name):
        rows.append((len(rows) + 1, parent, name))
        return len(rows)

    add(0, "Welcome Lobby")
    add(0, "AFK")
    stage = add(0, "Event Stage")
    add(stage, "Backstage")
    add(stage, "Q&A Queue")
    gaming = add(0, "Gaming")
    for game in GAMES:
        g = add(gaming, game)
        add(g, "Casual")
        add(g, "Ranked")
        squads = add(g, "Squads")
        for n in range(1, 5):
            squad = add(squads, f"Squad {n}")
            if n <= 2:
                add(squad, "Team Red")
                add(squad, "Team Blue")
    clans = add(0, "Clans")
    for clan in CLANS:
        c = add(clans, clan)
        add(c, "Lounge")
        ops = add(c, "Ops")
        for unit in ("Alpha", "Bravo", "Charlie"):
            add(ops, unit)
    community = add(0, "Community")
    for topic in TOPICS:
        t = add(community, topic)
        for room in ("Chat", "Voice 1", "Voice 2"):
            add(t, room)
    music = add(0, "Music")
    for room in ["Listening Party", "Jam Room 1", "Jam Room 2", "Jam Room 3", "Jam Room 4",
                 "Karaoke"]:
        add(music, room)
    study = add(0, "Study Hall")
    for room in [f"Silent {n}" for n in range(1, 11)] + ["Pomodoro"] + \
            [f"Group Study {n}" for n in range(1, 6)]:
        add(study, room)
    support = add(0, "Support")
    add(support, "Help Desk")
    add(support, "Waiting Room")
    staff = add(support, "Staff")
    add(staff, "Moderators")
    add(staff, "Admins")
    return rows


def main():
    print("begin;")
    for cid, parent, name in layout():
        quoted = name.replace("'", "''")
        print("insert or ignore into channels (server_id, channel_id, parent_id, name, inheritacl)"
              f" values (1, {cid}, {parent}, '{quoted}', 1);")
    print("commit;")


if __name__ == "__main__":
    main()
