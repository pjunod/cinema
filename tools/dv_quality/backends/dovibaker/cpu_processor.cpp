#include <cstdint>
#include <cstdio>
#include <fstream>
#include <iterator>
#include <string>
#include <vector>
#include "DoViProcessor.h"
const AVS_Linkage* AVS_linkage = nullptr;
static void write_le(std::ofstream& f, uint16_t value) {
  unsigned char b[] = {static_cast<unsigned char>(value & 255), static_cast<unsigned char>(value >> 8)};
  f.write(reinterpret_cast<const char*>(b), 2);
}
int main(int argc, char** argv) {
  if (argc != 5) return 64;
  std::ifstream input(argv[1], std::ios::binary);
  std::vector<uint8_t> nal((std::istreambuf_iterator<char>(input)), {});
  if (nal.empty()) return 65;
  int residual = std::stoi(argv[3]);
  int el_bits = std::stoi(argv[4]);
  DoViProcessor processor("", nullptr, 16, el_bits);
  if (!processor.wasCreationSuccessful()) return 66;
  if (!processor.intializeFrame(0, nullptr, nal.data(), nal.size())) return 67;
  std::ofstream output(argv[2], std::ios::binary);
  for (int y=0; y<16; ++y) {
    for (int x=0; x<16; ++x) {
      uint16_t bl[3] = {static_cast<uint16_t>((400 + 2*x + y) << 6), 32768, 32768};
      uint16_t el[3];
      for (int c=0; c<3; ++c) {
        int rr = residual ? ((x+y+c)%2 ? 16 : -16) : 0;
        el[c] = static_cast<uint16_t>((512+rr) << 6);
      }
      uint16_t dy = processor.processSampleY(bl[0],el[0]);
      uint16_t du = processor.processSampleU(bl[1],el[1],bl[0],bl[1],bl[2]);
      uint16_t dv = processor.processSampleV(bl[2],el[2],bl[0],bl[1],bl[2]);
      uint16_t r,g,b;
      processor.sample2rgb(r,g,b,dy,du,dv);
      write_le(output,r);write_le(output,g);write_le(output,b);
    }
  }
  if (!output) return 68;
  printf("{\"accepted\":true,\"el_processing\":%s,\"trim_processing\":%s,\"pixels\":256,\"format\":\"rgb48le\"}\n",
         processor.elProcessingEnabled()?"true":"false", processor.trimProcessingEnabled()?"true":"false");
}
