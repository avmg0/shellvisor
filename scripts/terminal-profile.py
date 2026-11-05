# Writes Shellvisor.terminal: Terminal.app profile with the design's colours.
import plistlib, sys
from plistlib import UID

def archive(root, classname, classes):
    objs = ['$null', dict(root, **{'$class': UID(2)}), {'$classname': classname, '$classes': classes}]
    return plistlib.dumps({'$version': 100000, '$archiver': 'NSKeyedArchiver',
                           '$top': {'root': UID(1)}, '$objects': objs}, fmt=plistlib.FMT_BINARY)

def color(hexv, alpha=None):
    r, g, b = (int(hexv[i:i+2], 16) / 255 for i in (0, 2, 4))
    s = ' '.join(f'{x:.10g}' for x in (r, g, b))
    if alpha is not None:
        s += f' {alpha:.10g}'
    # device RGB, like the colours Terminal saves when you type a hex value in sRGB
    return archive({'NSRGB': s.encode() + b'\0', 'NSColorSpace': 2}, 'NSColor', ['NSColor', 'NSObject'])

def font(name, size):
    objs = ['$null', {'NSName': UID(2), 'NSSize': float(size), 'NSfFlags': 16, '$class': UID(3)}, name,
            {'$classname': 'NSFont', '$classes': ['NSFont', 'NSObject']}]
    return plistlib.dumps({'$version': 100000, '$archiver': 'NSKeyedArchiver',
                           '$top': {'root': UID(1)}, '$objects': objs}, fmt=plistlib.FMT_BINARY)

P = {
    'BackgroundColor': '0B0E12', 'TextColor': 'D6DBE3', 'TextBoldColor': 'FFFFFF',
    'CursorColor': 'D6DBE3', 'SelectionColor': '252B35',
    'ANSIBlackColor': '1A1F27', 'ANSIRedColor': 'F07178', 'ANSIGreenColor': '8FD18F',
    'ANSIYellowColor': 'E0A84A', 'ANSIBlueColor': '82AAFF', 'ANSIMagentaColor': 'C792EA',
    'ANSICyanColor': '6CC9D6', 'ANSIWhiteColor': 'D6DBE3',
    'ANSIBrightBlackColor': '6B7482', 'ANSIBrightRedColor': 'FF8B92', 'ANSIBrightGreenColor': 'A8E0A8',
    'ANSIBrightYellowColor': 'F3C97A', 'ANSIBrightBlueColor': 'A3C1FF', 'ANSIBrightMagentaColor': 'D8B0F0',
    'ANSIBrightCyanColor': '8FDBE5', 'ANSIBrightWhiteColor': 'FFFFFF',
}
d = {k: color(v) for k, v in P.items()}
d.update({
    'Font': font('JetBrainsMono-Regular', 14), 'FontAntialias': True, 'UseBoldFonts': True,
    'UseBrightBold': False, 'DynamicANSIForegroundColors': False, 'BackgroundBlur': 0.0,
    'BackgroundAlphaInactive': 1.0, 'BackgroundSettingsForInactiveWindows': False,
    'CursorType': 0, 'columnCount': 160, 'rowCount': 44,
    'ProfileCurrentVersion': 2.09, 'name': 'Shellvisor', 'type': 'Window Settings',
})
with open(sys.argv[1], 'wb') as f:
    plistlib.dump(d, f, fmt=plistlib.FMT_XML, sort_keys=True)
