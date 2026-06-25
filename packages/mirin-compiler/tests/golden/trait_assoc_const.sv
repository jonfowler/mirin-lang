module bool__Bits__pack (
    input  logic self,
    output logic [0:0] result
);
    logic [0:0] __block_0;
    always_comb begin
        if (self) begin
            __block_0 = 1;
        end else begin
            __block_0 = 0;
        end
    end
    assign result = __block_0;
endmodule

module uint__Bits__pack (
    input  logic [7:0] self,
    output logic [7:0] result
);
    assign result = self;
endmodule

module top (
    input  logic clk,
    input  logic b,
    output logic [0:0] result
);
    packer__bool packer__bool (
        .x(b),
        .result(result)
    );
endmodule

module top_wide (
    input  logic clk,
    input  logic [7:0] w,
    output logic [7:0] result
);
    packer__uint8 packer__uint8 (
        .x(w),
        .result(result)
    );
endmodule

module packer__bool (
    input  logic x,
    output logic [0:0] result
);
    bool__Bits__pack bool__Bits__pack (
        .self(x),
        .result(result)
    );
endmodule

module packer__uint8 (
    input  logic [7:0] x,
    output logic [7:0] result
);
    uint__Bits__pack uint__Bits__pack (
        .self(x),
        .result(result)
    );
endmodule
