def test_arithmetic():
    assert 2 + 3 == 5


def test_normalized_inventory():
    inventory = {"wood": 3, "stone": 2}
    assert sum(inventory.values()) == 5
