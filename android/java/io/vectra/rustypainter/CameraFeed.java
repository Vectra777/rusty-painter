package io.vectra.rustypainter;

import android.app.Activity;
import android.graphics.ImageFormat;
import android.hardware.camera2.CameraAccessException;
import android.hardware.camera2.CameraCaptureSession;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraDevice;
import android.hardware.camera2.CameraManager;
import android.hardware.camera2.CaptureRequest;
import android.hardware.camera2.params.StreamConfigurationMap;
import android.media.Image;
import android.media.ImageReader;
import android.os.Handler;
import android.os.HandlerThread;
import android.util.Range;
import android.util.Size;
import android.view.Surface;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.ArrayList;
import java.util.Collections;

/**
 * The Hand Scanner's camera: Camera2 frames, small (about 320x240) and
 * about 15 a second, turned upright and converted to RGB into one direct
 * buffer the Rust side maps once. A frame is copied in only while the Rust
 * side isn't reading (begin/end), and a counter tells it a new one is
 * there; nothing is passed per frame but that number. Frames are as the
 * camera sees them (a front camera's not mirrored).
 *
 * The buffer: an 8-byte header (width and height, little-endian ints),
 * then the frame's RGB rows.
 */
public final class CameraFeed {
    private static final int HEADER = 8;
    private static final Object lock = new Object();
    private static CameraFeed current;

    private final Activity activity;
    private final String id;
    private final HandlerThread thread;
    private final Handler handler;
    private final ByteBuffer shared;
    private final int sensorOrientation;
    private final boolean front;
    private ImageReader reader;
    private CameraDevice device;
    private CameraCaptureSession session;
    private byte[] rgb = new byte[0];
    private byte[] y = new byte[0], u = new byte[0], v = new byte[0];
    private boolean reading;
    private long seq;
    private String error;

    private CameraFeed(Activity activity, String id, int capacity, int sensorOrientation,
            boolean front) {
        this.activity = activity;
        this.id = id;
        this.sensorOrientation = sensorOrientation;
        this.front = front;
        shared = ByteBuffer.allocateDirect(HEADER + capacity).order(ByteOrder.LITTLE_ENDIAN);
        thread = new HandlerThread("CameraFeed");
        thread.start();
        handler = new Handler(thread.getLooper());
    }

    /** The cameras, each as its id, a newline, and a name to show. */
    public static String[] list(Activity activity) throws CameraAccessException {
        CameraManager manager = activity.getSystemService(CameraManager.class);
        ArrayList<String> out = new ArrayList<>();
        int front = 0, back = 0, other = 0;
        for (String id : manager.getCameraIdList()) {
            Integer facing = manager.getCameraCharacteristics(id)
                    .get(CameraCharacteristics.LENS_FACING);
            String name;
            if (facing != null && facing == CameraCharacteristics.LENS_FACING_FRONT) {
                name = ++front == 1 ? "Front camera" : "Front camera " + front;
            } else if (facing != null && facing == CameraCharacteristics.LENS_FACING_BACK) {
                name = ++back == 1 ? "Back camera" : "Back camera " + back;
            } else {
                name = ++other == 1 ? "External camera" : "External camera " + other;
            }
            out.add(id + "\n" + name);
        }
        // Front cameras first: a hand is held up to the screen.
        Collections.sort(out, (a, b) -> Boolean.compare(!a.contains("\nFront"),
                !b.contains("\nFront")));
        return out.toArray(new String[0]);
    }

    /**
     * Open camera `id` at about `width` x `height` (any running one stops):
     * the buffer frames arrive in.
     */
    public static ByteBuffer start(Activity activity, String id, int width, int height)
            throws Exception {
        stop();
        CameraManager manager = activity.getSystemService(CameraManager.class);
        CameraCharacteristics info = manager.getCameraCharacteristics(id);
        StreamConfigurationMap map =
                info.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);
        if (map == null) throw new Exception("The camera gives no pictures");
        Size best = null;
        for (Size s : map.getOutputSizes(ImageFormat.YUV_420_888)) {
            long d = Math.abs((long) s.getWidth() * s.getHeight() - (long) width * height);
            if (best == null
                    || d < Math.abs((long) best.getWidth() * best.getHeight()
                            - (long) width * height)) {
                best = s;
            }
        }
        if (best == null) throw new Exception("The camera gives no pictures");
        Integer orientation = info.get(CameraCharacteristics.SENSOR_ORIENTATION);
        Integer facing = info.get(CameraCharacteristics.LENS_FACING);
        CameraFeed feed = new CameraFeed(activity, id, best.getWidth() * best.getHeight() * 3,
                orientation == null ? 0 : orientation,
                facing != null && facing == CameraCharacteristics.LENS_FACING_FRONT);
        Range<Integer> fps = null;
        Range<Integer>[] ranges = info.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES);
        if (ranges != null) {
            for (Range<Integer> r : ranges) {
                if (r.getLower() <= 15 && r.getUpper() >= 15
                        && (fps == null || r.getUpper() < fps.getUpper())) {
                    fps = r;
                }
            }
        }
        synchronized (lock) {
            current = feed;
        }
        feed.open(manager, best, fps);
        return feed.shared;
    }

    /** The newest frame's number if it's newer than `last`, the buffer then
     * held for reading until {@link #end}; else -1. */
    public static long begin(long last) {
        synchronized (lock) {
            CameraFeed feed = current;
            if (feed == null || feed.seq <= last) return -1;
            feed.reading = true;
            return feed.seq;
        }
    }

    /** Done reading: the next frame may come in. */
    public static void end() {
        synchronized (lock) {
            if (current != null) current.reading = false;
        }
    }

    /** What went wrong, if the camera stopped by itself. */
    public static String error() {
        synchronized (lock) {
            return current == null ? null : current.error;
        }
    }

    public static void stop() {
        CameraFeed feed;
        synchronized (lock) {
            feed = current;
            current = null;
        }
        if (feed != null) feed.close();
    }

    private void fail(String message) {
        synchronized (lock) {
            if (error == null) error = message;
        }
    }

    private void open(CameraManager manager, Size size, Range<Integer> fps)
            throws CameraAccessException {
        reader = ImageReader.newInstance(size.getWidth(), size.getHeight(),
                ImageFormat.YUV_420_888, 2);
        reader.setOnImageAvailableListener(r -> {
            Image image = r.acquireLatestImage();
            if (image == null) return;
            try {
                convert(image);
            } catch (RuntimeException e) {
                fail(e.toString());
            } finally {
                image.close();
            }
        }, handler);
        manager.openCamera(id, new CameraDevice.StateCallback() {
            @Override
            public void onOpened(CameraDevice camera) {
                device = camera;
                try {
                    camera.createCaptureSession(Collections.singletonList(reader.getSurface()),
                            new CameraCaptureSession.StateCallback() {
                                @Override
                                public void onConfigured(CameraCaptureSession s) {
                                    session = s;
                                    try {
                                        CaptureRequest.Builder request = camera.createCaptureRequest(
                                                CameraDevice.TEMPLATE_PREVIEW);
                                        request.addTarget(reader.getSurface());
                                        if (fps != null) {
                                            request.set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, fps);
                                        }
                                        s.setRepeatingRequest(request.build(), null, handler);
                                    } catch (Exception e) {
                                        fail(e.toString());
                                    }
                                }

                                @Override
                                public void onConfigureFailed(CameraCaptureSession s) {
                                    fail("The camera couldn't be set up");
                                }
                            }, handler);
                } catch (Exception e) {
                    fail(e.toString());
                }
            }

            @Override
            public void onDisconnected(CameraDevice camera) {
                camera.close();
                fail("Another app took the camera");
            }

            @Override
            public void onError(CameraDevice camera, int code) {
                camera.close();
                fail("The camera failed (" + code + ")");
            }
        }, handler);
    }

    private void close() {
        handler.post(() -> {
            try {
                if (session != null) session.close();
                if (device != null) device.close();
                if (reader != null) reader.close();
            } catch (RuntimeException e) {
                // Closing anyway.
            }
            thread.quitSafely();
        });
    }

    /** How far to turn the camera's frames (clockwise) to stand them up
     * on the screen as it's held now. */
    @SuppressWarnings("deprecation")
    private int rotation() {
        int display = 0;
        switch (activity.getWindowManager().getDefaultDisplay().getRotation()) {
            case Surface.ROTATION_90: display = 90; break;
            case Surface.ROTATION_180: display = 180; break;
            case Surface.ROTATION_270: display = 270; break;
            default: break;
        }
        return front ? (sensorOrientation + display) % 360
                : (sensorOrientation - display + 360) % 360;
    }

    private static byte[] read(Image.Plane plane, byte[] into) {
        ByteBuffer b = plane.getBuffer();
        if (into.length < b.remaining()) into = new byte[b.remaining()];
        b.get(into, 0, b.remaining());
        return into;
    }

    /** YUV (BT.601, video range) to RGB, turned upright. */
    private void convert(Image image) {
        int w = image.getWidth(), h = image.getHeight();
        Image.Plane[] planes = image.getPlanes();
        y = read(planes[0], y);
        u = read(planes[1], u);
        v = read(planes[2], v);
        int yRow = planes[0].getRowStride(), yPixel = planes[0].getPixelStride();
        int uvRow = planes[1].getRowStride(), uvPixel = planes[1].getPixelStride();
        int turn = rotation();
        boolean sideways = turn == 90 || turn == 270;
        int ow = sideways ? h : w, oh = sideways ? w : h;
        if (rgb.length != ow * oh * 3) rgb = new byte[ow * oh * 3];
        int o = 0;
        for (int oy = 0; oy < oh; oy++) {
            for (int ox = 0; ox < ow; ox++) {
                int sx, sy;
                switch (turn) {
                    case 90: sx = oy; sy = h - 1 - ox; break;
                    case 180: sx = w - 1 - ox; sy = h - 1 - oy; break;
                    case 270: sx = w - 1 - oy; sy = ox; break;
                    default: sx = ox; sy = oy; break;
                }
                int yy = (y[sy * yRow + sx * yPixel] & 0xff) - 16;
                int c = (sy >> 1) * uvRow + (sx >> 1) * uvPixel;
                int cb = (u[c] & 0xff) - 128, cr = (v[c] & 0xff) - 128;
                int l = 298 * Math.max(yy, 0);
                rgb[o++] = clamp((l + 409 * cr + 128) >> 8);
                rgb[o++] = clamp((l - 100 * cb - 208 * cr + 128) >> 8);
                rgb[o++] = clamp((l + 516 * cb + 128) >> 8);
            }
        }
        synchronized (lock) {
            if (current != this || reading) return;
            shared.putInt(0, ow);
            shared.putInt(4, oh);
            shared.position(HEADER);
            shared.put(rgb, 0, ow * oh * 3);
            shared.position(0);
            seq++;
        }
    }

    private static byte clamp(int x) {
        return (byte) (x < 0 ? 0 : x > 255 ? 255 : x);
    }
}
