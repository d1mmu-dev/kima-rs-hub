#include "essentia.h"
#include "algorithmfactory.h"
#include <vector>
#include <iostream>
#include <iomanip>
#include <memory>
using namespace essentia;
using namespace essentia::standard;
int main() {
  essentia::init();
  auto& factory = AlgorithmFactory::instance();
  for (int n : {1,255,256,257,512,513,1024}) {
    std::vector<Real> signal(n), frame, bands;
    for (int i=0;i<n;++i) signal[i] = static_cast<Real>((i%17)-8)/16.0f;
    std::unique_ptr<Algorithm> cutter(factory.create("FrameCutter", "frameSize", 512, "hopSize", 256, "startFromZero", false));
    std::unique_ptr<Algorithm> frontend(factory.create("TensorflowInputMusiCNN"));
    cutter->input("signal").set(signal); cutter->output("frame").set(frame);
    frontend->input("frame").set(frame); frontend->output("bands").set(bands);
    for (int f=0;;++f) {
      cutter->compute(); if (frame.empty()) break;
      frontend->compute();
      std::cout << n << ',' << f << ',' << std::setprecision(9) << frame[0] << ',' << frame[255] << ',' << frame[256] << ',' << frame[511];
      for (auto x : bands) std::cout << ',' << x;
      std::cout << '\n';
    }
  }
  essentia::shutdown();
}
