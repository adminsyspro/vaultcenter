"""Run inside the appliance image, no devices, network, privileges or lab mounts.

docker run --rm --network none --entrypoint python3 \
  -v "$PWD/deploy:/tests:ro" vaultcenter:live-media /tests/tests/check_mhvtl_cartridges.py
"""
from test_cartridges import CartridgeTests


def main():
    fixture = CartridgeTests()
    try:
        fixture.setUp()
        fixture.instance.runner = fixture.instance.run_mktape
        fixture.controller.enter('local isolated test')
        for media_type, barcode in [('LTO7', 'REAL001L7'), ('LTO8', 'REAL002L8')]:
            fixture.request.update(media_type=media_type,barcode=barcode)
            result = fixture.create()
            assert result['media_type'] == media_type
            assert result['capacity_bytes'] == 2147483648
            assert result['segment_size_mib'] == 256
            assert result['multipart_part_size_mib'] == 256
            assert not (fixture.media/barcode/'.vaultcenter-create-pending').exists()
            assert fixture.create() == result
        assert fixture.count() == 2
        fixture.controller.leave('local isolated test')
        print('Real mktape LTO7/LTO8 creation, MAM validation, exclusive placement, catalogue and retries: OK')
    finally:
        fixture.doCleanups()


if __name__ == '__main__': main()
