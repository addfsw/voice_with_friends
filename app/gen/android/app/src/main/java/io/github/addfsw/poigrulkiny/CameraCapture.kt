package io.github.addfsw.poigrulkiny

import android.util.Size
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.core.content.ContextCompat
import java.nio.ByteBuffer
import java.util.concurrent.Executors

/// Камера телефона для видео в комнате. Кадры (YUV 4:2:0, уже повёрнутые
/// как держат телефон) уходят в Rust (`app/src/android.rs`), там их сжимает
/// тот же кодер, что и на компьютере.
///
/// Камера привязана к жизни окна: свернули программу — CameraX сама
/// ставит камеру на паузу (у собеседников остаётся последний кадр), голос при
/// этом продолжает идти. Вернулись — камера снова снимает.
class CameraCapture(private val activity: MainActivity) {
  private val executor = Executors.newSingleThreadExecutor()
  private var provider: ProcessCameraProvider? = null
  @Volatile private var running = false

  /// Включить камеру: фронтальную или основную. Если такой нет — другую.
  fun start(front: Boolean) {
    running = true
    val future = ProcessCameraProvider.getInstance(activity)
    future.addListener({
      if (!running) return@addListener
      val provider = try {
        future.get()
      } catch (e: Exception) {
        return@addListener
      }
      this.provider = provider
      provider.unbindAll()
      // 640×480 хватает на оба слоя телефона (180p и 360p); больше — только
      // лишняя работа на масштабирование.
      val resolution = ResolutionSelector.Builder()
        .setResolutionStrategy(
          ResolutionStrategy(
            Size(640, 480),
            ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER
          )
        )
        .build()
      val analysis = ImageAnalysis.Builder()
        .setResolutionSelector(resolution)
        .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
        .setOutputImageFormat(ImageAnalysis.OUTPUT_IMAGE_FORMAT_YUV_420_888)
        .setOutputImageRotationEnabled(true)
        .build()
      analysis.setAnalyzer(executor) { image ->
        try {
          if (running) {
            val planes = image.planes
            nativeCameraFrame(
              image.width,
              image.height,
              planes[0].buffer,
              planes[0].rowStride,
              planes[1].buffer,
              planes[2].buffer,
              planes[1].rowStride,
              planes[1].pixelStride
            )
          }
        } finally {
          image.close()
        }
      }
      val wanted = if (front) CameraSelector.DEFAULT_FRONT_CAMERA else CameraSelector.DEFAULT_BACK_CAMERA
      val other = if (front) CameraSelector.DEFAULT_BACK_CAMERA else CameraSelector.DEFAULT_FRONT_CAMERA
      val selector = when {
        provider.hasCamera(wanted) -> wanted
        provider.hasCamera(other) -> other
        else -> return@addListener
      }
      try {
        provider.bindToLifecycle(activity, selector, analysis)
      } catch (e: Exception) {
        running = false
      }
    }, ContextCompat.getMainExecutor(activity))
  }

  fun stop() {
    running = false
    provider?.unbindAll()
  }

  /// Кадр в Rust: плоскость яркости и плоскости U, V (у V тот же шаг, что у U).
  private external fun nativeCameraFrame(
    width: Int,
    height: Int,
    y: ByteBuffer,
    yStride: Int,
    u: ByteBuffer,
    v: ByteBuffer,
    uvStride: Int,
    uvPixelStride: Int
  )
}
