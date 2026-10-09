import os

import pytest

from harness import load, run


@pytest.mark.hardware
def test_flash_and_exact_readback(request):
    path = os.environ.get('CH32RV_HW_CONFIG')
    if not path:
        pytest.skip('CH32RV_HW_CONFIG is not set: optional hardware contract')
    values = {name: os.environ.get(name) for name in ('CH32RV_TEST_TOOL', 'OEP_HW_LOCK', 'OEP_HW_RESULTS')}
    if not all(values.values()):
        pytest.fail('CH32RV_TEST_TOOL, OEP_HW_LOCK and OEP_HW_RESULTS must be explicit')
    report, artifact = run(load(path), values['CH32RV_TEST_TOOL'], values['OEP_HW_LOCK'], values['OEP_HW_RESULTS'])
    request.getfixturevalue('record_property')('oep_evidence', str(artifact.resolve()))
    assert report['status'] == 'passed', f"{report.get('error')}; cleanup={report.get('cleanup_error')}; {artifact}"
